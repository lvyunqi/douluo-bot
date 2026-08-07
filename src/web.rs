//! 管理 HTTP 服务的生命周期、会话边界与首批只读接口。

use std::{
    collections::HashMap,
    net::{SocketAddr, TcpListener as StdTcpListener},
    sync::{Arc, Mutex, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use getrandom::fill;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::{net::TcpListener, runtime::Builder as RuntimeBuilder, sync::oneshot};

use crate::{config::WebConfig, store::Store};

const SESSION_COOKIE: &str = "douluo_admin_session";
const SESSION_TTL: Duration = Duration::from_secs(15 * 60);
const MAX_SESSIONS: usize = 128;
const MAX_API_BODY_BYTES: usize = 1024;
const SERVER_START_TIMEOUT: Duration = Duration::from_secs(5);

/// 管理端当前拥有的最小角色集合；后续可扩展到独立管理员目录。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AdminRole {
    ContentAdmin,
}

impl AdminRole {
    fn allows(self, permission: AdminPermission) -> bool {
        matches!(
            (self, permission),
            (Self::ContentAdmin, AdminPermission::ContentRead)
        )
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::ContentAdmin => "content_admin",
        }
    }
}

/// API 路由显式声明所需权限，避免把管理员会话误当作无边界通行证。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AdminPermission {
    ContentRead,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AuthError {
    Unauthorized,
    Forbidden,
    ServiceUnavailable,
}

impl AuthError {
    fn into_response(self) -> Response {
        match self {
            Self::Unauthorized => api_error(StatusCode::UNAUTHORIZED, "unauthorized"),
            Self::Forbidden => api_error(StatusCode::FORBIDDEN, "forbidden"),
            Self::ServiceUnavailable => {
                api_error(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable")
            }
        }
    }
}

#[derive(Clone, Debug)]
struct AdminSession {
    role: AdminRole,
    csrf_token: String,
    expires_at: Instant,
}

/// 管理服务共享状态。数据库仍通过 Store API 访问，路由不直接写目录表。
struct ManagementState {
    store: Store,
    admin_secret_hash: [u8; 32],
    sessions: Mutex<HashMap<String, AdminSession>>,
    secure_cookie: bool,
}

impl ManagementState {
    fn new(store: Store, web_config: &WebConfig) -> Self {
        Self {
            store,
            admin_secret_hash: hash_secret(&web_config.admin_secret),
            sessions: Mutex::new(HashMap::new()),
            // 只有配置了 HTTPS 公开基址时才附加 Secure，避免本地回环 HTTP 无法建立会话。
            secure_cookie: !web_config.public_base_url.is_empty(),
        }
    }

    fn secret_matches(&self, supplied_secret: &str) -> bool {
        constant_time_equal(&self.admin_secret_hash, &hash_secret(supplied_secret))
    }
}

/// 在插件内部持有后台 Tokio runtime，避免 Tokio 类型越过动态 ABI 边界。
pub(crate) struct ManagementServer {
    listen_addr: SocketAddr,
    state: Arc<ManagementState>,
    shutdown_sender: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<Result<(), String>>>,
}

impl ManagementServer {
    /// 仅在明确启用管理端时监听端口，默认部署不创建后台线程。
    pub(crate) fn start_if_enabled(
        web_config: &WebConfig,
        store: Store,
    ) -> Result<Option<Self>, String> {
        if !web_config.enabled {
            return Ok(None);
        }
        Self::start(web_config, store).map(Some)
    }

    /// 同步完成端口绑定和线程就绪握手，确保插件 init 不会报告一个未启动的服务。
    pub(crate) fn start(web_config: &WebConfig, store: Store) -> Result<Self, String> {
        let listen_addr = web_config.socket_addr()?;
        let state = Arc::new(ManagementState::new(store, web_config));
        Self::start_with_state(listen_addr, state)
    }

    fn start_with_state(
        listen_addr: SocketAddr,
        state: Arc<ManagementState>,
    ) -> Result<Self, String> {
        let listener = StdTcpListener::bind(listen_addr)
            .map_err(|error| format!("绑定管理服务 {listen_addr} 失败：{error}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("设置管理服务非阻塞监听失败：{error}"))?;
        let local_addr = listener
            .local_addr()
            .map_err(|error| format!("读取管理服务监听地址失败：{error}"))?;
        let (shutdown_sender, shutdown_receiver) = oneshot::channel();
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let thread_state = state.clone();
        let thread = thread::Builder::new()
            .name("douluo-management-http".to_string())
            .spawn(move || {
                let runtime = match RuntimeBuilder::new_multi_thread()
                    .enable_io()
                    .enable_time()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let message = format!("创建管理服务 Tokio runtime 失败：{error}");
                        let _ = ready_sender.send(Err(message.clone()));
                        return Err(message);
                    }
                };
                runtime.block_on(async move {
                    let listener = match TcpListener::from_std(listener) {
                        Ok(listener) => listener,
                        Err(error) => {
                            let message = format!("接管管理服务监听器失败：{error}");
                            let _ = ready_sender.send(Err(message.clone()));
                            return Err(message);
                        }
                    };
                    let _ = ready_sender.send(Ok(()));
                    axum::serve(listener, build_router(thread_state))
                        .with_graceful_shutdown(async move {
                            let _ = shutdown_receiver.await;
                        })
                        .await
                        .map_err(|error| format!("管理服务异常停止：{error}"))
                })
            })
            .map_err(|error| format!("创建管理服务线程失败：{error}"))?;

        match ready_receiver.recv_timeout(SERVER_START_TIMEOUT) {
            Ok(Ok(())) => Ok(Self {
                listen_addr: local_addr,
                state,
                shutdown_sender: Some(shutdown_sender),
                thread: Some(thread),
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(error) => {
                let _ = shutdown_sender.send(());
                let _ = thread.join();
                Err(format!("等待管理服务启动超时：{error}"))
            }
        }
    }

    /// 停止后台服务并等待线程退出；服务错误不阻碍释放已经占用的端口。
    pub(crate) fn stop(&mut self) -> Result<(), String> {
        if let Some(sender) = self.shutdown_sender.take() {
            let _ = sender.send(());
        }
        if let Some(thread) = self.thread.take() {
            let result = thread
                .join()
                .map_err(|_| "管理服务线程异常退出".to_string())?;
            result?;
        }
        Ok(())
    }

    /// 复用已哈希的认证状态重新监听原地址，用于 reload 失败时恢复旧运行时。
    pub(crate) fn restart(&mut self) -> Result<(), String> {
        let listen_addr = self.listen_addr;
        let state = self.state.clone();
        self.stop()?;
        *self = Self::start_with_state(listen_addr, state)?;
        Ok(())
    }

    #[cfg(test)]
    fn local_addr(&self) -> SocketAddr {
        self.listen_addr
    }
}

impl Drop for ManagementServer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// 构建不含宽泛 CORS 的管理路由；写接口只会在 CSRF 校验路径中加入。
fn build_router(state: Arc<ManagementState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz).head(healthz))
        .route("/readyz", get(readyz).head(readyz))
        .route(
            "/api/v1/session",
            get(current_session).post(login).delete(logout),
        )
        .route("/api/v1/content/active", get(active_content_revision))
        .layer(DefaultBodyLimit::max(MAX_API_BODY_BYTES))
        .with_state(state)
}

async fn healthz() -> Response {
    plain_response(StatusCode::OK, "ok\n")
}

async fn readyz(State(state): State<Arc<ManagementState>>) -> Response {
    if state.store.active_content_revision().is_ok() {
        plain_response(StatusCode::OK, "ready\n")
    } else {
        plain_response(StatusCode::SERVICE_UNAVAILABLE, "not ready\n")
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginRequest {
    secret: String,
}

#[derive(Serialize)]
struct SessionResponse {
    role: &'static str,
    csrf_token: String,
    expires_in_seconds: u64,
}

async fn login(
    State(state): State<Arc<ManagementState>>,
    Json(request): Json<LoginRequest>,
) -> Response {
    if !state.secret_matches(&request.secret) {
        return api_error(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    let session_id = match random_token() {
        Ok(token) => token,
        Err(_) => return api_error(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable"),
    };
    let csrf_token = match random_token() {
        Ok(token) => token,
        Err(_) => return api_error(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable"),
    };
    let session = AdminSession {
        role: AdminRole::ContentAdmin,
        csrf_token: csrf_token.clone(),
        expires_at: Instant::now() + SESSION_TTL,
    };
    let mut sessions = match state.sessions.lock() {
        Ok(sessions) => sessions,
        Err(_) => return api_error(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable"),
    };
    purge_expired_sessions(&mut sessions);
    if sessions.len() >= MAX_SESSIONS {
        return api_error(StatusCode::TOO_MANY_REQUESTS, "session_capacity_reached");
    }
    sessions.insert(session_id.clone(), session);
    drop(sessions);

    let mut response = json_response(
        StatusCode::OK,
        SessionResponse {
            role: AdminRole::ContentAdmin.as_str(),
            csrf_token,
            expires_in_seconds: SESSION_TTL.as_secs(),
        },
    );
    let cookie = session_cookie(&session_id, state.secure_cookie);
    match HeaderValue::from_str(&cookie) {
        Ok(value) => {
            response.headers_mut().insert(header::SET_COOKIE, value);
            response
        }
        Err(_) => api_error(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable"),
    }
}

async fn current_session(
    State(state): State<Arc<ManagementState>>,
    headers: HeaderMap,
) -> Response {
    let (_, session) = match require_permission(&state, &headers, AdminPermission::ContentRead) {
        Ok(session) => session,
        Err(error) => return error.into_response(),
    };
    json_response(
        StatusCode::OK,
        SessionResponse {
            role: session.role.as_str(),
            csrf_token: session.csrf_token,
            expires_in_seconds: session
                .expires_at
                .saturating_duration_since(Instant::now())
                .as_secs(),
        },
    )
}

async fn logout(State(state): State<Arc<ManagementState>>, headers: HeaderMap) -> Response {
    let (session_id, session) =
        match require_permission(&state, &headers, AdminPermission::ContentRead) {
            Ok(session) => session,
            Err(error) => return error.into_response(),
        };
    if !csrf_matches(&session, &headers) {
        return api_error(StatusCode::FORBIDDEN, "csrf_required");
    }
    let mut sessions = match state.sessions.lock() {
        Ok(sessions) => sessions,
        Err(_) => return api_error(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable"),
    };
    sessions.remove(&session_id);
    drop(sessions);

    let mut response = secure_response(StatusCode::NO_CONTENT.into_response());
    let cookie = expired_session_cookie(state.secure_cookie);
    match HeaderValue::from_str(&cookie) {
        Ok(value) => {
            response.headers_mut().insert(header::SET_COOKIE, value);
            response
        }
        Err(_) => api_error(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable"),
    }
}

async fn active_content_revision(
    State(state): State<Arc<ManagementState>>,
    headers: HeaderMap,
) -> Response {
    if let Err(error) = require_permission(&state, &headers, AdminPermission::ContentRead) {
        return error.into_response();
    }
    match state.store.active_content_revision() {
        Ok(revision) => json_response(StatusCode::OK, json!({ "revision": revision })),
        Err(_) => api_error(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable"),
    }
}

fn require_permission(
    state: &ManagementState,
    headers: &HeaderMap,
    permission: AdminPermission,
) -> Result<(String, AdminSession), AuthError> {
    let session_id = session_id_from_headers(headers).ok_or(AuthError::Unauthorized)?;
    let mut sessions = state
        .sessions
        .lock()
        .map_err(|_| AuthError::ServiceUnavailable)?;
    purge_expired_sessions(&mut sessions);
    let session = sessions
        .get(&session_id)
        .cloned()
        .ok_or(AuthError::Unauthorized)?;
    if !session.role.allows(permission) {
        return Err(AuthError::Forbidden);
    }
    Ok((session_id, session))
}

fn csrf_matches(session: &AdminSession, headers: &HeaderMap) -> bool {
    let Some(value) = headers
        .get("x-csrf-token")
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    is_token(value) && constant_time_equal(session.csrf_token.as_bytes(), value.as_bytes())
}

fn session_id_from_headers(headers: &HeaderMap) -> Option<String> {
    let mut session_id = None;
    for header_value in headers.get_all(header::COOKIE).iter() {
        let raw_cookie = header_value.to_str().ok()?;
        for pair in raw_cookie.split(';') {
            let (name, value) = pair.trim().split_once('=')?;
            if name == SESSION_COOKIE && session_id.replace(value.to_string()).is_some() {
                return None;
            }
        }
    }
    session_id.filter(|value| is_token(value))
}

fn purge_expired_sessions(sessions: &mut HashMap<String, AdminSession>) {
    let now = Instant::now();
    sessions.retain(|_, session| session.expires_at > now);
}

fn hash_secret(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn random_token() -> Result<String, String> {
    let mut bytes = [0_u8; 32];
    fill(&mut bytes).map_err(|error| format!("生成管理会话随机数失败：{error}"))?;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut token = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        token.push(HEX[(byte >> 4) as usize] as char);
        token.push(HEX[(byte & 0x0f) as usize] as char);
    }
    Ok(token)
}

fn is_token(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn session_cookie(session_id: &str, secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!(
        "{SESSION_COOKIE}={session_id}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{}",
        SESSION_TTL.as_secs(),
        secure
    )
}

fn expired_session_cookie(secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!(
        "{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{}",
        secure
    )
}

fn plain_response(status: StatusCode, body: &'static str) -> Response {
    let mut response = secure_response((status, body).into_response());
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

fn json_response<T: Serialize>(status: StatusCode, value: T) -> Response {
    let mut response = Json(value).into_response();
    *response.status_mut() = status;
    secure_response(response)
}

fn api_error(status: StatusCode, code: &'static str) -> Response {
    json_response(status, json!({ "error": code }))
}

fn secure_response(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert(
        "content-security-policy",
        HeaderValue::from_static("default-src 'none'; base-uri 'none'; frame-ancestors 'none'"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;

    use axum::{
        body::{Body, to_bytes},
        http::{Method, Request},
    };
    use tempfile::TempDir;
    use tower::ServiceExt;

    fn state() -> (TempDir, Arc<ManagementState>) {
        let directory = tempfile::tempdir().expect("应创建管理服务临时目录");
        let store = Store::initialize(directory.path(), &crate::config::DatabaseConfig::default())
            .expect("应初始化管理服务数据库");
        let web_config = WebConfig {
            enabled: true,
            admin_secret: "0123456789abcdef".to_string(),
            ..WebConfig::default()
        };
        (
            directory,
            Arc::new(ManagementState::new(store, &web_config)),
        )
    }

    async fn request(
        app: &Router,
        method: Method,
        path: &str,
        headers: &[(&str, &str)],
        body: &'static [u8],
    ) -> Response {
        let mut request = Request::builder().method(method).uri(path);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        app.clone()
            .oneshot(request.body(Body::from(body)).expect("应构建请求"))
            .await
            .expect("路由应响应")
    }

    async fn response_json(response: Response) -> serde_json::Value {
        let bytes = to_bytes(response.into_body(), 16 * 1024)
            .await
            .expect("应读取响应正文");
        serde_json::from_slice(&bytes).expect("响应应为 JSON")
    }

    #[tokio::test]
    async fn health_and_ready_routes_are_minimal_and_cache_safe() {
        let (_directory, state) = state();
        let app = build_router(state);
        let response = request(&app, Method::GET, "/healthz", &[], b"").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
        let body = to_bytes(response.into_body(), 128)
            .await
            .expect("应读取健康检查");
        assert_eq!(body.as_ref(), b"ok\n");

        let response = request(&app, Method::GET, "/readyz", &[], b"").await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 128)
            .await
            .expect("应读取就绪检查");
        assert_eq!(body.as_ref(), b"ready\n");
    }

    #[tokio::test]
    async fn content_read_requires_session_and_logout_requires_csrf() {
        let (_directory, state) = state();
        let app = build_router(state);
        let response = request(&app, Method::GET, "/api/v1/content/active", &[], b"").await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = request(
            &app,
            Method::POST,
            "/api/v1/session",
            &[("content-type", "application/json")],
            br#"{"secret":"not-the-admin-secret"}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(response.headers().get(header::SET_COOKIE).is_none());

        let response = request(
            &app,
            Method::POST,
            "/api/v1/session",
            &[("content-type", "application/json")],
            br#"{"secret":"0123456789abcdef"}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .expect("登录应设置会话 cookie")
            .to_str()
            .expect("cookie 应为 ASCII")
            .split(';')
            .next()
            .expect("cookie 应有值")
            .to_string();
        let set_cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .expect("登录应设置会话 cookie")
            .to_str()
            .expect("cookie 应为 ASCII");
        assert!(set_cookie.contains("HttpOnly"));
        assert!(set_cookie.contains("SameSite=Strict"));
        let payload = response_json(response).await;
        let csrf_token = payload["csrf_token"]
            .as_str()
            .expect("登录应返回 CSRF token")
            .to_string();
        assert_eq!(payload["role"], "content_admin");

        let response = request(
            &app,
            Method::GET,
            "/api/v1/content/active",
            &[("cookie", &cookie)],
            b"",
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(payload["revision"]["package_key"], "douluo-core");

        let response = request(
            &app,
            Method::DELETE,
            "/api/v1/session",
            &[("cookie", &cookie)],
            b"",
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let response = request(
            &app,
            Method::DELETE,
            "/api/v1/session",
            &[("cookie", &cookie), ("x-csrf-token", &csrf_token)],
            b"",
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = request(
            &app,
            Method::GET,
            "/api/v1/content/active",
            &[("cookie", &cookie)],
            b"",
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn management_server_starts_and_stops_on_an_ephemeral_loopback_port() {
        let (directory, state) = state();
        let web_config = WebConfig {
            enabled: true,
            port: 0,
            admin_secret: "0123456789abcdef".to_string(),
            ..WebConfig::default()
        };
        let mut server =
            ManagementServer::start(&web_config, state.store.clone()).expect("管理服务应启动");
        assert!(server.local_addr().ip().is_loopback());
        assert_ne!(server.local_addr().port(), 0);
        let mut stream = TcpStream::connect(server.local_addr()).expect("应连接管理服务");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("应设置读取超时");
        stream
            .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .expect("应发送健康检查请求");
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .expect("应读取健康检查响应");
        assert!(response.starts_with("HTTP/1.1 200"));
        assert!(response.ends_with("ok\n"));
        server.stop().expect("管理服务应停止");
        server.restart().expect("管理服务应重新启动");
        assert!(server.local_addr().ip().is_loopback());
        assert_ne!(server.local_addr().port(), 0);
        server.stop().expect("重新启动的管理服务应停止");
        drop(directory);
    }
}
