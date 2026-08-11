//! 管理 HTTP 服务的生命周期、会话边界与首批只读接口。

use std::{
    collections::HashMap,
    fs,
    net::{SocketAddr, TcpListener as StdTcpListener},
    path::{Path as FsPath, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use getrandom::fill;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::{net::TcpListener, runtime::Builder as RuntimeBuilder, sync::oneshot};

use crate::{
    catalog,
    config::{WebConfig, is_safe_data_relative_path},
    content::{is_content_key, load_package_file},
    embedded_web::ManagementWebAssets,
    player_stage_confirmation::{
        PlayerStageCandidate, list_player_stage_candidates, load_player_stage_candidate,
    },
    store::{
        ContentAdminAuditActor, ContentAdminDraftStageOperationRecord, ContentAdminOperationRecord,
        ContentAdminRollbackOperationRecord, ContentDraftDiffMember, ContentDraftRecord,
        ContentRevisionActivationRecord, ContentRevisionRecord, ContentValidationReport,
        LEGACY_CLAIM_REQUIRED, PlayerStageConfirmationReceipt, Store,
    },
};

const SESSION_COOKIE: &str = "douluo_admin_session";
const SESSION_TTL: Duration = Duration::from_secs(15 * 60);
const MAX_SESSIONS: usize = 128;
const MAX_API_BODY_BYTES: usize = 1024;
const SERVER_START_TIMEOUT: Duration = Duration::from_secs(5);
const MANAGEMENT_PAGE_CSP: &str = "default-src 'none'; base-uri 'none'; connect-src 'self'; font-src 'self'; frame-ancestors 'none'; form-action 'self'; script-src 'self'; style-src 'self'";

/// 管理端当前拥有的最小角色集合；后续可扩展到独立管理员目录。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AdminRole {
    ContentAdmin,
}

impl AdminRole {
    fn allows(self, permission: AdminPermission) -> bool {
        matches!(
            (self, permission),
            (
                Self::ContentAdmin,
                AdminPermission::ContentRead
                    | AdminPermission::ContentWrite
                    | AdminPermission::PlayerStageRead
                    | AdminPermission::PlayerStageWrite
            )
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
    ContentWrite,
    PlayerStageRead,
    PlayerStageWrite,
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
    data_dir: PathBuf,
    admin_secret_hash: [u8; 32],
    sessions: Mutex<HashMap<String, AdminSession>>,
    secure_cookie: bool,
}

impl ManagementState {
    fn new(store: Store, web_config: &WebConfig, data_dir: &FsPath) -> Result<Self, String> {
        let data_dir = fs::canonicalize(data_dir)
            .map_err(|error| format!("解析管理内容根目录失败：{error}"))?;
        if !data_dir.is_dir() {
            return Err("管理内容根目录必须是目录".to_string());
        }
        Ok(Self {
            store,
            data_dir,
            admin_secret_hash: hash_secret(&web_config.admin_secret),
            sessions: Mutex::new(HashMap::new()),
            // 只有配置了 HTTPS 公开基址时才附加 Secure，避免本地回环 HTTP 无法建立会话。
            secure_cookie: !web_config.public_base_url.is_empty(),
        })
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
        data_dir: &FsPath,
    ) -> Result<Option<Self>, String> {
        if !web_config.enabled {
            return Ok(None);
        }
        Self::start(web_config, store, data_dir).map(Some)
    }

    /// 同步完成端口绑定和线程就绪握手，确保插件 init 不会报告一个未启动的服务。
    pub(crate) fn start(
        web_config: &WebConfig,
        store: Store,
        data_dir: &FsPath,
    ) -> Result<Self, String> {
        let listen_addr = web_config.socket_addr()?;
        let state = Arc::new(ManagementState::new(store, web_config, data_dir)?);
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
        .route("/api/v1/illustrations", get(illustration_bindings))
        .route("/api/v1/content/active", get(active_content_revision))
        .route("/api/v1/content/revisions", get(content_revisions))
        .route("/api/v1/content/drafts", get(content_drafts))
        .route("/api/v1/content/drafts/stage", post(stage_content_draft))
        .route(
            "/api/v1/player-staging/candidates",
            get(player_stage_candidates),
        )
        .route("/api/v1/player-staging/confirm", post(confirm_player_stage))
        .route(
            "/api/v1/content/drafts/{package_key}/{package_revision}/diff",
            get(content_draft_diff),
        )
        .route(
            "/api/v1/content/drafts/{package_key}/{package_revision}/validate",
            post(validate_content_draft),
        )
        .route(
            "/api/v1/content/drafts/{package_key}/{package_revision}/publish",
            post(publish_content_draft),
        )
        .route(
            "/api/v1/content/revisions/{revision_id}/rollback",
            post(rollback_content_revision),
        )
        .route("/api/v1/content/activations", get(content_activations))
        .route("/api/v1/content/operations", get(content_admin_operations))
        .route(
            "/api/v1/content/rollback-operations",
            get(content_admin_rollback_operations),
        )
        .route(
            "/api/v1/content/stage-operations",
            get(content_admin_draft_stage_operations),
        )
        .route("/", get(management_page))
        .route("/assets/{*asset_path}", get(management_asset))
        .layer(DefaultBodyLimit::max(MAX_API_BODY_BYTES))
        .with_state(state)
}

/// 返回打包进动态插件的管理端登录页面，不为未知路径提供 SPA 回退。
async fn management_page() -> Response {
    embedded_asset_response("index.html")
}

/// 只接受 Vite 产物中的资源路径，避免静态入口成为任意文件读取接口。
async fn management_asset(Path(asset_path): Path<String>) -> Response {
    if asset_path.is_empty()
        || asset_path
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
    {
        return static_not_found_response();
    }
    embedded_asset_response(&format!("assets/{asset_path}"))
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

/// 仅引用 data_dir 内已由部署面放置的内容文件，管理 API 不接收内容正文。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ContentDraftStageRequest {
    package_file: String,
}

/// 只接收受限 data_dir 内的 stage 文件与单条源角色 ID，不接受角色正文。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlayerStageConfirmRequest {
    stage_file: String,
    source_player_id: i64,
}

/// 管理端读取 stage 候选时使用独立游标，避免把外部源角色 ID 当成数据库主键。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlayerStageCandidatesQuery {
    stage_file: String,
    #[serde(default)]
    after_source_player_id: Option<i64>,
    #[serde(default = "default_content_page_limit")]
    limit: usize,
}

#[derive(Serialize)]
struct SessionResponse {
    role: &'static str,
    csrf_token: String,
    expires_in_seconds: u64,
}

/// 只读插图目录刻意不暴露直连地址、本地路径或媒体服务内部元数据。
#[derive(Serialize)]
struct IllustrationBindingListEntry {
    entity_type: String,
    entity_key: String,
    media_role: String,
    asset_key: String,
    alt: String,
    width: u16,
    height: u16,
}

#[derive(Serialize)]
struct IllustrationBindingsResponse {
    entries: Vec<IllustrationBindingListEntry>,
}

/// 管理列表统一使用受限的自增 ID 游标，避免无界 offset 查询。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ContentPageQuery {
    #[serde(default)]
    after_id: Option<i64>,
    #[serde(default = "default_content_page_limit")]
    limit: usize,
}

fn default_content_page_limit() -> usize {
    25
}

#[derive(Serialize)]
struct CursorPage<T> {
    entries: Vec<T>,
    next_after_id: Option<i64>,
}

#[derive(Serialize)]
struct ContentRevisionListEntry {
    revision: ContentRevisionRecord,
    member_count: i64,
}

#[derive(Serialize)]
struct ContentDraftListEntry {
    id: i64,
    package_key: String,
    package_revision: i64,
    source_format: String,
    content_hash: String,
    status: String,
    validation_errors: Vec<String>,
    published_revision_id: Option<i64>,
    created_at: i64,
    updated_at: i64,
}

#[derive(Serialize)]
struct ContentActivationListEntry {
    id: i64,
    revision_id: i64,
    reason: String,
    created_at: i64,
}

#[derive(Serialize)]
/// 草稿差异预览 API 中不含正文的新增目录成员。
struct ContentDraftDiffMemberResponse {
    member_kind: String,
    member_key: String,
}

#[derive(Serialize)]
/// 草稿差异预览 API 的只读响应，避免传出草稿正文。
struct ContentDraftDiffResponse {
    draft: ContentDraftListEntry,
    active_revision: ContentRevisionRecord,
    active_member_count: i64,
    added_members: Vec<ContentDraftDiffMemberResponse>,
    projected_member_count: i64,
}

/// 草稿校验结果只返回可运营的摘要，不回传草稿正文。
#[derive(Serialize)]
struct ContentValidationResponse {
    package_key: String,
    package_revision: i64,
    content_hash: String,
    valid: bool,
    errors: Vec<String>,
    item_count: usize,
    wuhun_count: usize,
    skill_count: usize,
    effect_count: usize,
    soul_beast_count: usize,
    soul_ring_count: usize,
}

#[derive(Serialize)]
struct ContentPublishResponse {
    revision: ContentRevisionRecord,
    active_revision_id: i64,
    member_count: i64,
    replayed: bool,
}

#[derive(Serialize)]
struct ContentDraftStageResponse {
    draft: ContentDraftListEntry,
    replayed: bool,
}

#[derive(Serialize)]
struct ContentRollbackResponse {
    revision: ContentRevisionRecord,
    active_revision_id: i64,
    activation_id: i64,
}

/// 候选列表只返回确认所需的基础资料，不返回来源路径、来源摘要或校验问题正文。
#[derive(Serialize)]
struct PlayerStageCandidateListEntry {
    source_player_id: i64,
    subject_id: String,
    name: String,
    gender: String,
    level: i64,
    exp: i64,
    hp: i64,
    max_hp: i64,
    soul_power: i64,
    max_soul_power: i64,
    strength: i64,
    agility: i64,
    spirit: i64,
    endurance: i64,
    perception: i64,
    luck: i64,
    life_count: i64,
}

#[derive(Serialize)]
struct PlayerStageCandidatesResponse {
    protocol: String,
    account_id: String,
    namespace: String,
    staged_at: i64,
    total_players: i64,
    ready_players: i64,
    rejected_players: i64,
    entries: Vec<PlayerStageCandidateListEntry>,
    next_after_source_player_id: Option<i64>,
}

#[derive(Serialize)]
struct PlayerStageConfirmationResponse {
    player_id: i64,
    source_player_id: i64,
    name: String,
    level: i64,
    map_name: String,
}

/// 内容管理员审计 API 不返回会话指纹，避免把会话关联信息扩散到页面响应。
#[derive(Serialize)]
struct ContentAdminOperationListEntry {
    id: i64,
    actor_role: String,
    action: String,
    package_key: String,
    package_revision: i64,
    content_hash: String,
    outcome: String,
    revision_id: Option<i64>,
    created_at: i64,
}

/// 回滚审计列表不返回会话指纹，只暴露回滚目标与 activation 元数据。
#[derive(Serialize)]
struct ContentAdminRollbackOperationListEntry {
    id: i64,
    actor_role: String,
    revision_id: i64,
    activation_id: i64,
    created_at: i64,
}

/// 暂存审计列表不返回文件路径或会话指纹，只暴露草稿快照与结果。
#[derive(Serialize)]
struct ContentAdminDraftStageOperationListEntry {
    id: i64,
    actor_role: String,
    package_key: String,
    package_revision: i64,
    content_hash: String,
    source_format: String,
    outcome: String,
    created_at: i64,
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

/// 返回编译期 manifest 中的脱敏绑定，供管理员核对实体与稳定资源键。
async fn illustration_bindings(
    State(state): State<Arc<ManagementState>>,
    headers: HeaderMap,
) -> Response {
    if let Err(error) = require_permission(&state, &headers, AdminPermission::ContentRead) {
        return error.into_response();
    }
    let bindings = match catalog::bindings() {
        Ok(bindings) => bindings,
        Err(_) => return api_error(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable"),
    };
    let entries = bindings
        .iter()
        .map(|binding| IllustrationBindingListEntry {
            entity_type: binding.entity_type.clone(),
            entity_key: binding.entity_key.clone(),
            media_role: binding.media_role.clone(),
            asset_key: binding.asset_key.clone(),
            alt: binding.alt.clone(),
            width: binding.display.width,
            height: binding.display.height,
        })
        .collect();
    json_response(StatusCode::OK, IllustrationBindingsResponse { entries })
}

async fn content_revisions(
    State(state): State<Arc<ManagementState>>,
    headers: HeaderMap,
    Query(query): Query<ContentPageQuery>,
) -> Response {
    if let Err(error) = require_permission(&state, &headers, AdminPermission::ContentRead) {
        return error.into_response();
    }
    let (after_id, limit) = match content_page_params(query) {
        Ok(params) => params,
        Err(code) => return api_error(StatusCode::BAD_REQUEST, code),
    };
    match state.store.list_content_revisions(after_id, limit) {
        Ok(page) => json_response(
            StatusCode::OK,
            CursorPage {
                entries: page
                    .entries
                    .into_iter()
                    .map(|entry| ContentRevisionListEntry {
                        revision: entry.revision,
                        member_count: entry.member_count,
                    })
                    .collect(),
                next_after_id: page.next_after_id,
            },
        ),
        Err(_) => api_error(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable"),
    }
}

async fn content_drafts(
    State(state): State<Arc<ManagementState>>,
    headers: HeaderMap,
    Query(query): Query<ContentPageQuery>,
) -> Response {
    if let Err(error) = require_permission(&state, &headers, AdminPermission::ContentRead) {
        return error.into_response();
    }
    let (after_id, limit) = match content_page_params(query) {
        Ok(params) => params,
        Err(code) => return api_error(StatusCode::BAD_REQUEST, code),
    };
    let page = match state.store.list_content_drafts(after_id, limit) {
        Ok(page) => page,
        Err(_) => return api_error(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable"),
    };
    let entries = match page
        .entries
        .into_iter()
        .map(content_draft_list_entry)
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(entries) => entries,
        Err(_) => return api_error(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable"),
    };
    json_response(
        StatusCode::OK,
        CursorPage {
            entries,
            next_after_id: page.next_after_id,
        },
    )
}

/// 在认证后的只读快照中返回草稿目录差异，不改变任何发布状态。
async fn content_draft_diff(
    State(state): State<Arc<ManagementState>>,
    headers: HeaderMap,
    Path((package_key, package_revision)): Path<(String, i64)>,
) -> Response {
    if let Err(error) = require_permission(&state, &headers, AdminPermission::ContentRead) {
        return error.into_response();
    }
    if package_revision <= 0 {
        return api_error(StatusCode::BAD_REQUEST, "invalid_draft_identity");
    }
    let preview = match state
        .store
        .preview_content_draft(&package_key, package_revision)
    {
        Ok(Some(preview)) => preview,
        Ok(None) => return api_error(StatusCode::NOT_FOUND, "not_found"),
        Err(_) => return api_error(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable"),
    };
    let draft = match content_draft_list_entry(preview.draft) {
        Ok(draft) => draft,
        Err(_) => return api_error(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable"),
    };
    json_response(
        StatusCode::OK,
        ContentDraftDiffResponse {
            draft,
            active_revision: preview.active_revision,
            active_member_count: preview.active_member_count,
            added_members: preview
                .added_members
                .into_iter()
                .map(content_draft_diff_member_response)
                .collect(),
            projected_member_count: preview.projected_member_count,
        },
    )
}

/// 暂存 data_dir 内受控内容文件；请求只传安全相对路径，不接收内容正文。
async fn stage_content_draft(
    State(state): State<Arc<ManagementState>>,
    headers: HeaderMap,
    Json(request): Json<ContentDraftStageRequest>,
) -> Response {
    let (session_id, session) =
        match require_permission(&state, &headers, AdminPermission::ContentWrite) {
            Ok(session) => session,
            Err(error) => return error.into_response(),
        };
    if !csrf_matches(&session, &headers) {
        return api_error(StatusCode::FORBIDDEN, "csrf_required");
    }
    if !valid_content_package_file_path(&request.package_file) {
        return api_error(StatusCode::BAD_REQUEST, "invalid_package_file");
    }
    let loaded = match load_package_file(&state.data_dir, &request.package_file) {
        Ok(loaded) => loaded,
        Err(_) => return api_error(StatusCode::UNPROCESSABLE_ENTITY, "invalid_package_file"),
    };
    let session_fingerprint = session_audit_fingerprint(&session_id);
    let actor = ContentAdminAuditActor {
        role: session.role.as_str(),
        session_fingerprint: &session_fingerprint,
    };
    match state.store.stage_content_package_as_admin(&loaded, actor) {
        Ok(receipt) => match content_draft_list_entry(receipt.draft) {
            Ok(draft) => json_response(
                StatusCode::OK,
                ContentDraftStageResponse {
                    draft,
                    replayed: receipt.replayed,
                },
            ),
            Err(_) => api_error(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable"),
        },
        Err(error) => content_stage_error(&error),
    }
}

/// 读取 data_dir 内已部署的 v42.1 stage 候选；不写入 stage 或目标角色库。
async fn player_stage_candidates(
    State(state): State<Arc<ManagementState>>,
    headers: HeaderMap,
    Query(query): Query<PlayerStageCandidatesQuery>,
) -> Response {
    if let Err(error) = require_permission(&state, &headers, AdminPermission::PlayerStageRead) {
        return error.into_response();
    }
    let (after_source_player_id, limit) = match player_stage_page_params(&query) {
        Ok(params) => params,
        Err(code) => return api_error(StatusCode::BAD_REQUEST, code),
    };
    if !valid_player_stage_file_path(&query.stage_file) {
        return api_error(StatusCode::BAD_REQUEST, "invalid_player_stage_file");
    }
    match list_player_stage_candidates(
        &state.data_dir,
        &query.stage_file,
        after_source_player_id,
        limit,
    ) {
        Ok(page) => json_response(
            StatusCode::OK,
            PlayerStageCandidatesResponse {
                protocol: page.protocol,
                account_id: page.account_id,
                namespace: page.namespace,
                staged_at: page.staged_at,
                total_players: page.total_players,
                ready_players: page.ready_players,
                rejected_players: page.rejected_players,
                entries: page
                    .entries
                    .into_iter()
                    .map(player_stage_candidate_list_entry)
                    .collect(),
                next_after_source_player_id: page.next_after_source_player_id,
            },
        ),
        Err(_) => api_error(StatusCode::UNPROCESSABLE_ENTITY, "invalid_player_stage"),
    }
}

/// 从已验证 stage 重新读取单条候选，并由 Store 在同一事务中创建角色和确认审计。
async fn confirm_player_stage(
    State(state): State<Arc<ManagementState>>,
    headers: HeaderMap,
    Json(request): Json<PlayerStageConfirmRequest>,
) -> Response {
    let (session_id, session) =
        match require_permission(&state, &headers, AdminPermission::PlayerStageWrite) {
            Ok(session) => session,
            Err(error) => return error.into_response(),
        };
    if !csrf_matches(&session, &headers) {
        return api_error(StatusCode::FORBIDDEN, "csrf_required");
    }
    if request.source_player_id <= 0 || !valid_player_stage_file_path(&request.stage_file) {
        return api_error(StatusCode::BAD_REQUEST, "invalid_player_stage_request");
    }
    let candidate = match load_player_stage_candidate(
        &state.data_dir,
        &request.stage_file,
        request.source_player_id,
    ) {
        Ok(candidate) => candidate,
        Err(error) if error == "玩家 staging 候选不存在" => {
            return api_error(StatusCode::NOT_FOUND, "not_found");
        }
        Err(_) => return api_error(StatusCode::UNPROCESSABLE_ENTITY, "invalid_player_stage"),
    };
    let session_fingerprint = session_audit_fingerprint(&session_id);
    let actor = ContentAdminAuditActor {
        role: session.role.as_str(),
        session_fingerprint: &session_fingerprint,
    };
    match state.store.confirm_player_stage_as_admin(&candidate, actor) {
        Ok(receipt) => json_response(
            StatusCode::CREATED,
            player_stage_confirmation_response(receipt),
        ),
        Err(error) => player_stage_confirmation_error(&error),
    }
}

/// 校验已暂存草稿；请求不携带正文，避免 HTTP 层形成目录直写入口。
async fn validate_content_draft(
    State(state): State<Arc<ManagementState>>,
    headers: HeaderMap,
    Path((package_key, package_revision)): Path<(String, i64)>,
) -> Response {
    let (session_id, session) =
        match require_permission(&state, &headers, AdminPermission::ContentWrite) {
            Ok(session) => session,
            Err(error) => return error.into_response(),
        };
    if !csrf_matches(&session, &headers) {
        return api_error(StatusCode::FORBIDDEN, "csrf_required");
    }
    if !valid_content_draft_identity(&package_key, package_revision) {
        return api_error(StatusCode::BAD_REQUEST, "invalid_draft_identity");
    }
    let session_fingerprint = session_audit_fingerprint(&session_id);
    let actor = ContentAdminAuditActor {
        role: session.role.as_str(),
        session_fingerprint: &session_fingerprint,
    };
    match state
        .store
        .validate_content_draft_as_admin(&package_key, package_revision, actor)
    {
        Ok(report) => json_response(StatusCode::OK, content_validation_response(report)),
        Err(error) => content_write_error(&error),
    }
}

/// 发布已校验草稿；Store 在同一写事务中复核目录、激活 revision 并追加审计。
async fn publish_content_draft(
    State(state): State<Arc<ManagementState>>,
    headers: HeaderMap,
    Path((package_key, package_revision)): Path<(String, i64)>,
) -> Response {
    let (session_id, session) =
        match require_permission(&state, &headers, AdminPermission::ContentWrite) {
            Ok(session) => session,
            Err(error) => return error.into_response(),
        };
    if !csrf_matches(&session, &headers) {
        return api_error(StatusCode::FORBIDDEN, "csrf_required");
    }
    if !valid_content_draft_identity(&package_key, package_revision) {
        return api_error(StatusCode::BAD_REQUEST, "invalid_draft_identity");
    }
    let session_fingerprint = session_audit_fingerprint(&session_id);
    let actor = ContentAdminAuditActor {
        role: session.role.as_str(),
        session_fingerprint: &session_fingerprint,
    };
    match state
        .store
        .publish_content_draft_as_admin(&package_key, package_revision, actor)
    {
        Ok(receipt) => {
            let status = if receipt.replayed {
                StatusCode::OK
            } else {
                StatusCode::CREATED
            };
            json_response(
                status,
                ContentPublishResponse {
                    revision: receipt.revision,
                    active_revision_id: receipt.active_revision_id,
                    member_count: receipt.member_count,
                    replayed: receipt.replayed,
                },
            )
        }
        Err(error) => content_write_error(&error),
    }
}

/// 显式回滚到既有内容 revision；Store 只追加 rollback activation 与管理员审计。
async fn rollback_content_revision(
    State(state): State<Arc<ManagementState>>,
    headers: HeaderMap,
    Path(revision_id): Path<i64>,
) -> Response {
    let (session_id, session) =
        match require_permission(&state, &headers, AdminPermission::ContentWrite) {
            Ok(session) => session,
            Err(error) => return error.into_response(),
        };
    if !csrf_matches(&session, &headers) {
        return api_error(StatusCode::FORBIDDEN, "csrf_required");
    }
    if revision_id <= 0 {
        return api_error(StatusCode::BAD_REQUEST, "invalid_revision_id");
    }
    let session_fingerprint = session_audit_fingerprint(&session_id);
    let actor = ContentAdminAuditActor {
        role: session.role.as_str(),
        session_fingerprint: &session_fingerprint,
    };
    match state
        .store
        .rollback_content_revision_as_admin(revision_id, actor)
    {
        Ok(receipt) => json_response(
            StatusCode::CREATED,
            ContentRollbackResponse {
                active_revision_id: receipt.revision.id,
                revision: receipt.revision,
                activation_id: receipt.activation_id,
            },
        ),
        Err(error) => content_rollback_error(&error),
    }
}

async fn content_activations(
    State(state): State<Arc<ManagementState>>,
    headers: HeaderMap,
    Query(query): Query<ContentPageQuery>,
) -> Response {
    if let Err(error) = require_permission(&state, &headers, AdminPermission::ContentRead) {
        return error.into_response();
    }
    let (after_id, limit) = match content_page_params(query) {
        Ok(params) => params,
        Err(code) => return api_error(StatusCode::BAD_REQUEST, code),
    };
    match state.store.list_content_activations(after_id, limit) {
        Ok(page) => json_response(
            StatusCode::OK,
            CursorPage {
                entries: page
                    .entries
                    .into_iter()
                    .map(content_activation_list_entry)
                    .collect(),
                next_after_id: page.next_after_id,
            },
        ),
        Err(_) => api_error(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable"),
    }
}

async fn content_admin_operations(
    State(state): State<Arc<ManagementState>>,
    headers: HeaderMap,
    Query(query): Query<ContentPageQuery>,
) -> Response {
    if let Err(error) = require_permission(&state, &headers, AdminPermission::ContentRead) {
        return error.into_response();
    }
    let (after_id, limit) = match content_page_params(query) {
        Ok(params) => params,
        Err(code) => return api_error(StatusCode::BAD_REQUEST, code),
    };
    match state.store.list_content_admin_operations(after_id, limit) {
        Ok(page) => json_response(
            StatusCode::OK,
            CursorPage {
                entries: page
                    .entries
                    .into_iter()
                    .map(content_admin_operation_list_entry)
                    .collect(),
                next_after_id: page.next_after_id,
            },
        ),
        Err(_) => api_error(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable"),
    }
}

async fn content_admin_rollback_operations(
    State(state): State<Arc<ManagementState>>,
    headers: HeaderMap,
    Query(query): Query<ContentPageQuery>,
) -> Response {
    if let Err(error) = require_permission(&state, &headers, AdminPermission::ContentRead) {
        return error.into_response();
    }
    let (after_id, limit) = match content_page_params(query) {
        Ok(params) => params,
        Err(code) => return api_error(StatusCode::BAD_REQUEST, code),
    };
    match state
        .store
        .list_content_admin_rollback_operations(after_id, limit)
    {
        Ok(page) => json_response(
            StatusCode::OK,
            CursorPage {
                entries: page
                    .entries
                    .into_iter()
                    .map(content_admin_rollback_operation_list_entry)
                    .collect(),
                next_after_id: page.next_after_id,
            },
        ),
        Err(_) => api_error(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable"),
    }
}

async fn content_admin_draft_stage_operations(
    State(state): State<Arc<ManagementState>>,
    headers: HeaderMap,
    Query(query): Query<ContentPageQuery>,
) -> Response {
    if let Err(error) = require_permission(&state, &headers, AdminPermission::ContentRead) {
        return error.into_response();
    }
    let (after_id, limit) = match content_page_params(query) {
        Ok(params) => params,
        Err(code) => return api_error(StatusCode::BAD_REQUEST, code),
    };
    match state
        .store
        .list_content_admin_draft_stage_operations(after_id, limit)
    {
        Ok(page) => json_response(
            StatusCode::OK,
            CursorPage {
                entries: page
                    .entries
                    .into_iter()
                    .map(content_admin_draft_stage_operation_list_entry)
                    .collect(),
                next_after_id: page.next_after_id,
            },
        ),
        Err(_) => api_error(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable"),
    }
}

fn content_page_params(query: ContentPageQuery) -> Result<(Option<i64>, usize), &'static str> {
    if query.after_id.is_some_and(|after_id| after_id < 0) || !(1..=100).contains(&query.limit) {
        return Err("invalid_pagination");
    }
    Ok((query.after_id, query.limit))
}

fn player_stage_page_params(
    query: &PlayerStageCandidatesQuery,
) -> Result<(Option<i64>, usize), &'static str> {
    if query
        .after_source_player_id
        .is_some_and(|source_player_id| source_player_id < 0)
        || !(1..=100).contains(&query.limit)
    {
        return Err("invalid_pagination");
    }
    Ok((query.after_source_player_id, query.limit))
}

fn content_draft_list_entry(
    draft: ContentDraftRecord,
) -> Result<ContentDraftListEntry, serde_json::Error> {
    Ok(ContentDraftListEntry {
        id: draft.id,
        package_key: draft.package_key,
        package_revision: draft.package_revision,
        source_format: draft.source_format,
        content_hash: draft.content_hash,
        status: draft.status,
        validation_errors: serde_json::from_str(&draft.validation_json)?,
        published_revision_id: draft.published_revision_id,
        created_at: draft.created_at,
        updated_at: draft.updated_at,
    })
}

fn player_stage_candidate_list_entry(
    candidate: PlayerStageCandidate,
) -> PlayerStageCandidateListEntry {
    PlayerStageCandidateListEntry {
        source_player_id: candidate.source_player_id,
        subject_id: candidate.subject_id,
        name: candidate.name,
        gender: candidate.gender,
        level: candidate.level,
        exp: candidate.exp,
        hp: candidate.hp,
        max_hp: candidate.max_hp,
        soul_power: candidate.soul_power,
        max_soul_power: candidate.max_soul_power,
        strength: candidate.strength,
        agility: candidate.agility,
        spirit: candidate.spirit,
        endurance: candidate.endurance,
        perception: candidate.perception,
        luck: candidate.luck,
        life_count: candidate.life_count,
    }
}

fn player_stage_confirmation_response(
    receipt: PlayerStageConfirmationReceipt,
) -> PlayerStageConfirmationResponse {
    PlayerStageConfirmationResponse {
        player_id: receipt.player_id,
        source_player_id: receipt.source_player_id,
        name: receipt.name,
        level: receipt.level,
        map_name: receipt.map_name,
    }
}

fn content_activation_list_entry(
    activation: ContentRevisionActivationRecord,
) -> ContentActivationListEntry {
    ContentActivationListEntry {
        id: activation.id,
        revision_id: activation.revision_id,
        reason: activation.reason,
        created_at: activation.created_at,
    }
}

fn content_draft_diff_member_response(
    member: ContentDraftDiffMember,
) -> ContentDraftDiffMemberResponse {
    ContentDraftDiffMemberResponse {
        member_kind: member.member_kind,
        member_key: member.member_key,
    }
}

fn content_validation_response(report: ContentValidationReport) -> ContentValidationResponse {
    let valid = report.errors.is_empty();
    ContentValidationResponse {
        package_key: report.package_key,
        package_revision: report.package_revision,
        content_hash: report.content_hash,
        valid,
        errors: report.errors,
        item_count: report.item_count,
        wuhun_count: report.wuhun_count,
        skill_count: report.skill_count,
        effect_count: report.effect_count,
        soul_beast_count: report.soul_beast_count,
        soul_ring_count: report.soul_ring_count,
    }
}

fn content_admin_operation_list_entry(
    operation: ContentAdminOperationRecord,
) -> ContentAdminOperationListEntry {
    ContentAdminOperationListEntry {
        id: operation.id,
        actor_role: operation.actor_role,
        action: operation.action,
        package_key: operation.package_key,
        package_revision: operation.package_revision,
        content_hash: operation.content_hash,
        outcome: operation.outcome,
        revision_id: operation.revision_id,
        created_at: operation.created_at,
    }
}

fn content_admin_rollback_operation_list_entry(
    operation: ContentAdminRollbackOperationRecord,
) -> ContentAdminRollbackOperationListEntry {
    ContentAdminRollbackOperationListEntry {
        id: operation.id,
        actor_role: operation.actor_role,
        revision_id: operation.revision_id,
        activation_id: operation.activation_id,
        created_at: operation.created_at,
    }
}

fn content_admin_draft_stage_operation_list_entry(
    operation: ContentAdminDraftStageOperationRecord,
) -> ContentAdminDraftStageOperationListEntry {
    ContentAdminDraftStageOperationListEntry {
        id: operation.id,
        actor_role: operation.actor_role,
        package_key: operation.package_key,
        package_revision: operation.package_revision,
        content_hash: operation.content_hash,
        source_format: operation.source_format,
        outcome: operation.outcome,
        created_at: operation.created_at,
    }
}

fn valid_content_draft_identity(package_key: &str, package_revision: i64) -> bool {
    package_revision > 0 && is_content_key(package_key)
}

fn valid_content_package_file_path(package_file: &str) -> bool {
    if !is_safe_data_relative_path(package_file) {
        return false;
    }
    matches!(
        FsPath::new(package_file)
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref(),
        Some("json" | "toml")
    )
}

fn valid_player_stage_file_path(stage_file: &str) -> bool {
    is_safe_data_relative_path(stage_file)
        && FsPath::new(stage_file)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("sqlite"))
}

/// 只按稳定错误类别返回写操作失败，避免把 SQLite 或目录内部细节暴露给管理端。
fn content_write_error(error: &str) -> Response {
    match error {
        "内容草稿不存在" => api_error(StatusCode::NOT_FOUND, "not_found"),
        "内容草稿必须先通过验证才能发布" => {
            api_error(StatusCode::CONFLICT, "draft_not_validated")
        }
        _ if error.starts_with("内容草稿校验失败：") => {
            api_error(StatusCode::UNPROCESSABLE_ENTITY, "draft_rejected")
        }
        _ => api_error(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable"),
    }
}

/// 回滚路由只公开稳定错误类别，避免泄露 SQLite 或内容目录细节。
fn content_rollback_error(error: &str) -> Response {
    match error {
        "目标内容 revision 不存在" => api_error(StatusCode::NOT_FOUND, "not_found"),
        _ => api_error(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable"),
    }
}

/// 暂存文件失败时只返回稳定类别，避免把 data_dir 结构或解析细节泄露给管理端。
fn content_stage_error(error: &str) -> Response {
    if error.starts_with("内容包 ") && error.contains("已发布且内容不同") {
        api_error(StatusCode::CONFLICT, "published_draft_conflict")
    } else if error.starts_with("内容包字段校验失败：")
        || error == "内容包哈希与规范化内容不一致"
        || error == "内容包来源格式必须是 json 或 toml"
    {
        api_error(StatusCode::UNPROCESSABLE_ENTITY, "invalid_package")
    } else {
        api_error(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable")
    }
}

/// stage 错误不暴露路径、SQLite 细节或来源资料，冲突与不可确认状态保持稳定类别。
fn player_stage_confirmation_error(error: &str) -> Response {
    match error {
        "目标玩家身份已存在" | "玩家 staging 候选已经确认过" | LEGACY_CLAIM_REQUIRED => {
            api_error(StatusCode::CONFLICT, "player_stage_conflict")
        }
        "玩家 staging 等级与经验曲线不一致" | "玩家 staging 状态不可确认" => {
            api_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "player_stage_not_confirmable",
            )
        }
        _ => api_error(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable"),
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

/// 审计只保存会话哈希，避免把可用的 HttpOnly cookie 值写入数据库。
fn session_audit_fingerprint(session_id: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut fingerprint = String::with_capacity(64);
    for byte in hash_secret(session_id) {
        fingerprint.push(HEX[(byte >> 4) as usize] as char);
        fingerprint.push(HEX[(byte & 0x0f) as usize] as char);
    }
    fingerprint
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

/// 从 DLL 内嵌资源中返回精确文件，缺失资源不会降级为页面或目录读取。
fn embedded_asset_response(path: &str) -> Response {
    let Some(asset) = ManagementWebAssets::get(path) else {
        return static_not_found_response();
    };
    let mut response = secure_response((StatusCode::OK, asset.data.into_owned()).into_response());
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(embedded_asset_content_type(path)),
    );
    response.headers_mut().insert(
        "content-security-policy",
        HeaderValue::from_static(MANAGEMENT_PAGE_CSP),
    );
    response
}

fn static_not_found_response() -> Response {
    plain_response(StatusCode::NOT_FOUND, "not found\n")
}

fn embedded_asset_content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, extension)| extension) {
        Some("css") => "text/css; charset=utf-8",
        Some("html") => "text/html; charset=utf-8",
        Some("ico") => "image/x-icon",
        Some("jpeg" | "jpg") => "image/jpeg",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("webp") => "image/webp",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
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

    use crate::content::{
        ContentPackage, EffectPackageEntry, LoadedContentPackage, SoulBeastPackageEntry,
        SoulBeastSkillPoolPackageEntry, StatePackageEntry, content_hash,
    };
    use crate::player_staging::{PlayerStagingMetadata, stage_recent_sqlite_player_profiles};
    use axum::{
        body::{Body, to_bytes},
        http::{Method, Request},
    };
    use rusqlite::Connection;
    use std::fs;
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
        let state = Arc::new(
            ManagementState::new(store, &web_config, directory.path()).expect("应创建管理服务状态"),
        );
        (directory, state)
    }

    fn create_player_stage_file(data_dir: &std::path::Path) {
        let source_path = data_dir.join("web-recent.sqlite");
        let source = Connection::open(&source_path).expect("应创建 Web stage 源库");
        source
            .execute_batch(
                r#"
                CREATE TABLE player(
                    id INTEGER PRIMARY KEY,
                    user_id INTEGER,
                    name TEXT,
                    nickname TEXT,
                    sex TEXT,
                    level INTEGER,
                    exp INTEGER,
                    hp INTEGER,
                    max_hp INTEGER,
                    mp INTEGER,
                    max_mp INTEGER,
                    strength INTEGER,
                    agility INTEGER,
                    spirit INTEGER,
                    endurance INTEGER,
                    perception INTEGER,
                    luck INTEGER,
                    life_count INTEGER,
                    state INTEGER
                );
                INSERT INTO player VALUES(
                    1, 30001, 'Web确认角色', NULL, '女', 1, 0, 100, 100, 50, 50,
                    12, 13, 14, 15, 16, 17, 1, 0
                );
                "#,
            )
            .expect("应写入 Web stage 源资料");
        drop(source);
        stage_recent_sqlite_player_profiles(
            &source_path,
            &data_dir.join("web-stage.sqlite"),
            &PlayerStagingMetadata {
                protocol: "onebot11".to_string(),
                account_id: "10001".to_string(),
                namespace: "default".to_string(),
            },
        )
        .expect("应生成 Web stage 文件");
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

    async fn login_for_test(app: &Router) -> (String, String) {
        let response = request(
            app,
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
        let payload = response_json(response).await;
        let csrf_token = payload["csrf_token"]
            .as_str()
            .expect("登录应返回 CSRF token")
            .to_string();
        (cookie, csrf_token)
    }

    fn effect_package(
        package_key: &str,
        effect_key: &str,
        skill_key: &str,
    ) -> LoadedContentPackage {
        let package = ContentPackage {
            package_key: package_key.to_string(),
            revision: 1,
            author: "web-test".to_string(),
            minimum_runtime: String::new(),
            maps: Vec::new(),
            items: Vec::new(),
            npcs: Vec::new(),
            quests: Vec::new(),
            numeric_curves: Vec::new(),
            states: Vec::new(),
            wuhun: Vec::new(),
            skills: Vec::new(),
            effects: vec![EffectPackageEntry {
                effect_key: effect_key.to_string(),
                skill_key: skill_key.to_string(),
                trigger_kind: "on_release".to_string(),
                target_kind: "enemy".to_string(),
                operation: "modify_stat".to_string(),
                attribute_key: "beast_attack".to_string(),
                value_mode: "percent_delta".to_string(),
                value: -10,
                duration_rounds: 1,
                chance_percent: 100,
                stack_policy: "strongest".to_string(),
                parameters: Default::default(),
                description: "web write test effect".to_string(),
                enabled: true,
            }],
            soul_beasts: Vec::new(),
            soul_beast_skill_pools: Vec::new(),
            soul_rings: Vec::new(),
            transitions: Vec::new(),
        };
        LoadedContentPackage {
            content_hash: content_hash(&package).expect("应计算 web 写入内容包哈希"),
            package,
            source_format: "json".to_string(),
        }
    }

    fn state_and_beast_skill_pool_package(package_key: &str) -> LoadedContentPackage {
        let package = ContentPackage {
            package_key: package_key.to_string(),
            revision: 1,
            author: "web-v41-test".to_string(),
            minimum_runtime: String::new(),
            maps: Vec::new(),
            items: Vec::new(),
            npcs: Vec::new(),
            quests: Vec::new(),
            numeric_curves: Vec::new(),
            states: vec![StatePackageEntry {
                state_key: "web-v41-action-lock".to_string(),
                name: "Web v41 行动锁".to_string(),
                state_kind: "action_lock".to_string(),
                target_kind: "beast".to_string(),
                settlement_phase: "before_player_action".to_string(),
                duration_rounds: 2,
                stack_policy: "refresh".to_string(),
                max_stacks: 1,
                dispellable: false,
                immunity_kind: "none".to_string(),
                description: "仅用于验证管理端状态目录差异。".to_string(),
            }],
            wuhun: Vec::new(),
            skills: Vec::new(),
            effects: Vec::new(),
            soul_beasts: vec![SoulBeastPackageEntry {
                beast_key: "web-v41-pool-beast".to_string(),
                name: "Web v41 技能池魂兽".to_string(),
                description: "仅用于验证管理端魂兽技能池差异。".to_string(),
                map_key: "sunset-forest".to_string(),
                age: 30,
                level_required: 1,
                max_hp: 30,
                attack: 4,
                defense: 1,
                speed: 9,
                exp_reward: 30,
                drop_item_key: "small-healing-potion".to_string(),
                drop_quantity: 1,
                enabled: true,
            }],
            soul_beast_skill_pools: vec![SoulBeastSkillPoolPackageEntry {
                beast_key: "web-v41-pool-beast".to_string(),
                skill_key: "entangle".to_string(),
                weight: 100,
                sort_order: 0,
            }],
            soul_rings: Vec::new(),
            transitions: Vec::new(),
        };
        LoadedContentPackage {
            content_hash: content_hash(&package).expect("应计算 v41 web 测试内容包哈希"),
            package,
            source_format: "json".to_string(),
        }
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
    async fn embedded_management_spa_is_cache_safe_and_preserves_api_boundaries() {
        let (_directory, state) = state();
        let app = build_router(state);

        let response = request(&app, Method::GET, "/", &[], b"").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
        assert_eq!(
            response.headers().get("content-security-policy").unwrap(),
            MANAGEMENT_PAGE_CSP
        );
        assert_eq!(
            response
                .headers()
                .get(header::X_CONTENT_TYPE_OPTIONS)
                .unwrap(),
            "nosniff"
        );
        assert_eq!(response.headers().get("x-frame-options").unwrap(), "DENY");
        assert_eq!(
            response.headers().get("referrer-policy").unwrap(),
            "no-referrer"
        );
        let body = to_bytes(response.into_body(), 16 * 1024)
            .await
            .expect("应读取内置管理页面");
        let page = std::str::from_utf8(&body).expect("内置管理页面应为 UTF-8");
        assert!(page.contains("<div id=\"root\"></div>"));
        assert!(!page.contains("0123456789abcdef"));
        let bundle_path = page
            .split('\"')
            .find(|value| value.starts_with("/assets/") && value.ends_with(".js"))
            .expect("页面应引用哈希 JavaScript bundle");

        let response = request(&app, Method::GET, bundle_path, &[], b"").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/javascript; charset=utf-8"
        );

        let response = request(&app, Method::GET, "/assets/missing.js", &[], b"").await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let response = request(&app, Method::GET, "/api/v1/content/drafts", &[], b"").await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
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

    #[tokio::test]
    async fn illustration_directory_requires_session_and_redacts_binding_sources() {
        let (_directory, state) = state();
        let app = build_router(state);

        let response = request(&app, Method::GET, "/api/v1/illustrations", &[], b"").await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let (cookie, _csrf_token) = login_for_test(&app).await;
        let response = request(
            &app,
            Method::GET,
            "/api/v1/illustrations",
            &[("cookie", &cookie)],
            b"",
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
        let payload = response_json(response).await;
        let entries = payload["entries"].as_array().expect("应返回插图目录");
        assert_eq!(entries.len(), 19);
        let holy_soul = entries
            .iter()
            .find(|entry| entry["asset_key"] == "maps/holy-soul-village/cover.webp")
            .expect("应返回圣魂村绑定");
        assert_eq!(holy_soul["entity_type"], "map");
        assert_eq!(holy_soul["entity_key"], "圣魂村");
        assert_eq!(holy_soul["media_role"], "cover");
        assert_eq!(holy_soul["alt"], "圣魂村地图");
        assert_eq!(holy_soul["width"], 640);
        assert_eq!(holy_soul["height"], 360);
        for entry in entries {
            assert!(entry.get("direct_url").is_none());
            assert!(entry.get("local_path").is_none());
            assert!(entry.get("storage_key").is_none());
            assert!(entry.get("sha256").is_none());
        }
    }

    #[tokio::test]
    async fn content_metadata_lists_are_authenticated_and_cursor_bounded() {
        let (_directory, state) = state();
        let package = ContentPackage {
            package_key: "web-list-draft".to_string(),
            revision: 1,
            author: "web-test".to_string(),
            minimum_runtime: String::new(),
            maps: Vec::new(),
            items: Vec::new(),
            npcs: Vec::new(),
            quests: Vec::new(),
            numeric_curves: Vec::new(),
            states: Vec::new(),
            wuhun: Vec::new(),
            skills: Vec::new(),
            effects: vec![EffectPackageEntry {
                effect_key: "web-list-effect".to_string(),
                skill_key: "missing-web-skill".to_string(),
                trigger_kind: "on_release".to_string(),
                target_kind: "enemy".to_string(),
                operation: "modify_stat".to_string(),
                attribute_key: "beast_attack".to_string(),
                value_mode: "percent_delta".to_string(),
                value: -10,
                duration_rounds: 1,
                chance_percent: 100,
                stack_policy: "strongest".to_string(),
                parameters: Default::default(),
                description: "web list test effect".to_string(),
                enabled: true,
            }],
            soul_beasts: Vec::new(),
            soul_beast_skill_pools: Vec::new(),
            soul_rings: Vec::new(),
            transitions: Vec::new(),
        };
        let loaded = LoadedContentPackage {
            content_hash: content_hash(&package).expect("应计算 web 测试内容包哈希"),
            package,
            source_format: "json".to_string(),
        };
        state
            .store
            .stage_content_package(&loaded)
            .expect("应写入 web 测试草稿");
        state
            .store
            .validate_content_draft("web-list-draft", 1)
            .expect("应校验 web 测试草稿");

        let app = build_router(state.clone());
        for path in [
            "/api/v1/content/revisions",
            "/api/v1/content/drafts",
            "/api/v1/content/drafts/web-list-draft/1/diff",
            "/api/v1/content/activations",
        ] {
            let response = request(&app, Method::GET, path, &[], b"").await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
        }

        let (cookie, _csrf_token) = login_for_test(&app).await;
        let headers = [("cookie", cookie.as_str())];

        let response = request(
            &app,
            Method::GET,
            "/api/v1/content/revisions?limit=1",
            &headers,
            b"",
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(payload["entries"].as_array().unwrap().len(), 1);
        assert_eq!(
            payload["entries"][0]["revision"]["package_key"],
            "douluo-core"
        );
        assert!(payload["entries"][0]["member_count"].as_i64().unwrap() > 0);

        let response = request(
            &app,
            Method::GET,
            "/api/v1/content/activations?limit=1",
            &headers,
            b"",
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(payload["entries"][0]["reason"], "initial");
        assert_eq!(payload["entries"][0]["revision_id"], 1);

        let response = request(
            &app,
            Method::GET,
            "/api/v1/content/drafts?limit=1",
            &headers,
            b"",
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(payload["entries"][0]["package_key"], "web-list-draft");
        assert_eq!(payload["entries"][0]["status"], "rejected");
        assert_eq!(
            payload["entries"][0]["validation_errors"][0],
            "效果 web-list-effect 引用了不存在或未启用的魂技 missing-web-skill"
        );
        assert_eq!(
            payload["entries"][0]["content_hash"]
                .as_str()
                .unwrap()
                .len(),
            64
        );
        assert!(payload["entries"][0].get("package_json").is_none());
        assert!(payload["entries"][0].get("validation_json").is_none());

        let response = request(
            &app,
            Method::GET,
            "/api/v1/content/drafts/web-list-draft/1/diff",
            &headers,
            b"",
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(payload["draft"]["package_key"], "web-list-draft");
        assert_eq!(payload["active_revision"]["package_key"], "douluo-core");
        assert_eq!(payload["added_members"].as_array().unwrap().len(), 2);
        assert!(
            payload["added_members"]
                .as_array()
                .unwrap()
                .iter()
                .any(|member| member["member_key"] == "web-list-effect")
        );
        assert!(payload.get("package_json").is_none());
        assert!(payload["draft"].get("package_json").is_none());

        let v41_package = state_and_beast_skill_pool_package("web-v41-diff-draft");
        state
            .store
            .stage_content_package(&v41_package)
            .expect("应写入 v41 web 测试草稿");
        state
            .store
            .validate_content_draft("web-v41-diff-draft", 1)
            .expect("应校验 v41 web 测试草稿");
        let response = request(
            &app,
            Method::GET,
            "/api/v1/content/drafts/web-v41-diff-draft/1/diff",
            &headers,
            b"",
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(payload["draft"]["package_key"], "web-v41-diff-draft");
        for (member_kind, member_key) in [
            ("state", "web-v41-action-lock"),
            ("beast-skill", "web-v41-pool-beast:entangle"),
        ] {
            assert!(
                payload["added_members"]
                    .as_array()
                    .expect("差异响应应包含成员数组")
                    .iter()
                    .any(|member| {
                        member["member_kind"] == member_kind && member["member_key"] == member_key
                    }),
                "v41 差异响应缺少成员 {member_kind} / {member_key}"
            );
        }
        assert!(payload.get("package_json").is_none());
        assert!(payload["draft"].get("package_json").is_none());

        let response = request(
            &app,
            Method::GET,
            "/api/v1/content/drafts/missing-web-draft/1/diff",
            &headers,
            b"",
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let payload = response_json(response).await;
        assert_eq!(payload["error"], "not_found");

        for path in [
            "/api/v1/content/revisions?limit=0",
            "/api/v1/content/drafts?after_id=-1",
            "/api/v1/content/activations?limit=101",
        ] {
            let response = request(&app, Method::GET, path, &headers, b"").await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
            let payload = response_json(response).await;
            assert_eq!(payload["error"], "invalid_pagination");
        }

        let response = request(
            &app,
            Method::GET,
            "/api/v1/content/drafts/web-list-draft/0/diff",
            &headers,
            b"",
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let payload = response_json(response).await;
        assert_eq!(payload["error"], "invalid_draft_identity");
    }

    #[tokio::test]
    async fn content_write_routes_require_csrf_and_append_atomic_audits() {
        let (_directory, state) = state();
        let valid = effect_package("web-write-valid", "web-write-effect", "entangle");
        let rejected = effect_package(
            "web-write-rejected",
            "web-write-rejected-effect",
            "missing-web-write-skill",
        );
        state
            .store
            .stage_content_package(&valid)
            .expect("应写入可发布 Web 草稿");
        state
            .store
            .stage_content_package(&rejected)
            .expect("应写入将被拒绝的 Web 草稿");
        let app = build_router(state);
        let validate_path = "/api/v1/content/drafts/web-write-valid/1/validate";
        let publish_path = "/api/v1/content/drafts/web-write-valid/1/publish";

        let response = request(&app, Method::POST, validate_path, &[], b"").await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let (cookie, csrf_token) = login_for_test(&app).await;
        let read_headers = [("cookie", cookie.as_str())];
        let write_headers = [
            ("cookie", cookie.as_str()),
            ("x-csrf-token", csrf_token.as_str()),
        ];
        let response = request(&app, Method::POST, validate_path, &read_headers, b"").await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let response = request(&app, Method::POST, validate_path, &write_headers, b"").await;
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(payload["package_key"], "web-write-valid");
        assert_eq!(payload["valid"], true);
        assert!(payload["errors"].as_array().unwrap().is_empty());
        assert!(payload.get("package_json").is_none());

        let response = request(&app, Method::POST, publish_path, &read_headers, b"").await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let response = request(&app, Method::POST, publish_path, &write_headers, b"").await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let payload = response_json(response).await;
        assert_eq!(payload["replayed"], false);
        assert_eq!(payload["revision"]["package_key"], "web-write-valid");

        let response = request(&app, Method::POST, publish_path, &write_headers, b"").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response_json(response).await["replayed"], true);

        let response = request(
            &app,
            Method::POST,
            "/api/v1/content/drafts/Web-Write-Invalid/1/validate",
            &write_headers,
            b"",
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response_json(response).await["error"],
            "invalid_draft_identity"
        );

        let response = request(
            &app,
            Method::POST,
            "/api/v1/content/drafts/web-write-rejected/1/validate",
            &write_headers,
            b"",
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(payload["valid"], false);
        assert!(!payload["errors"].as_array().unwrap().is_empty());

        let response = request(&app, Method::GET, "/api/v1/content/operations", &[], b"").await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let response = request(
            &app,
            Method::GET,
            "/api/v1/content/operations?limit=4",
            &read_headers,
            b"",
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        let entries = payload["entries"].as_array().expect("应返回审计列表");
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0]["action"], "validate");
        assert_eq!(entries[0]["outcome"], "validated");
        assert_eq!(entries[1]["action"], "publish");
        assert_eq!(entries[1]["outcome"], "published");
        assert_eq!(entries[2]["outcome"], "replayed");
        assert_eq!(entries[3]["outcome"], "rejected");
        assert!(
            entries
                .iter()
                .all(|entry| entry.get("actor_fingerprint").is_none())
        );

        let response = request(
            &app,
            Method::GET,
            "/api/v1/content/operations?limit=0",
            &read_headers,
            b"",
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response_json(response).await["error"], "invalid_pagination");
    }

    #[tokio::test]
    async fn content_stage_route_requires_csrf_and_uses_constrained_file_input() {
        let (directory, state) = state();
        let package = effect_package("web-stage", "web-stage-effect", "entangle");
        fs::write(
            directory.path().join("web-stage.json"),
            serde_json::to_vec(&package.package).expect("应序列化 Web 暂存内容包"),
        )
        .expect("应写入受控 Web 暂存文件");
        let app = build_router(state.clone());
        let stage_path = "/api/v1/content/drafts/stage";
        let stage_body = br#"{"package_file":"web-stage.json"}"#;

        let response = request(
            &app,
            Method::POST,
            stage_path,
            &[("content-type", "application/json")],
            stage_body,
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let (cookie, csrf_token) = login_for_test(&app).await;
        let read_headers = [("cookie", cookie.as_str())];
        let csrf_missing_headers = [
            ("cookie", cookie.as_str()),
            ("content-type", "application/json"),
        ];
        let write_headers = [
            ("cookie", cookie.as_str()),
            ("x-csrf-token", csrf_token.as_str()),
            ("content-type", "application/json"),
        ];
        let response = request(
            &app,
            Method::POST,
            stage_path,
            &csrf_missing_headers,
            stage_body,
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let response = request(
            &app,
            Method::POST,
            stage_path,
            &write_headers,
            br#"{"package_file":"../outside.json"}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response_json(response).await["error"],
            "invalid_package_file"
        );

        let response = request(
            &app,
            Method::POST,
            stage_path,
            &write_headers,
            br#"{"package_file":"web-stage.txt"}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response_json(response).await["error"],
            "invalid_package_file"
        );

        let response = request(
            &app,
            Method::POST,
            stage_path,
            &write_headers,
            br#"{"package_file":"missing.json"}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            response_json(response).await["error"],
            "invalid_package_file"
        );

        let response = request(&app, Method::POST, stage_path, &write_headers, stage_body).await;
        assert_eq!(response.status(), StatusCode::OK);
        let first = response_json(response).await;
        assert_eq!(first["draft"]["package_key"], "web-stage");
        assert_eq!(first["draft"]["status"], "draft");
        assert_eq!(first["replayed"], false);
        assert!(first["draft"].get("package_json").is_none());

        let response = request(&app, Method::POST, stage_path, &write_headers, stage_body).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response_json(response).await["replayed"], false);

        let response = request(
            &app,
            Method::GET,
            "/api/v1/content/stage-operations",
            &[],
            b"",
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let response = request(
            &app,
            Method::GET,
            "/api/v1/content/stage-operations?limit=2",
            &read_headers,
            b"",
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        let entries = payload["entries"].as_array().expect("应返回暂存审计列表");
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|entry| {
            entry["actor_role"] == "content_admin"
                && entry["package_key"] == "web-stage"
                && entry["outcome"] == "staged"
                && entry.get("actor_fingerprint").is_none()
        }));

        assert!(
            state
                .store
                .validate_content_draft("web-stage", 1)
                .expect("应校验 Web 暂存草稿")
                .errors
                .is_empty()
        );
        state
            .store
            .publish_content_draft("web-stage", 1)
            .expect("应发布 Web 暂存草稿");
        let response = request(&app, Method::POST, stage_path, &write_headers, stage_body).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response_json(response).await["replayed"], true);

        let response = request(
            &app,
            Method::GET,
            "/api/v1/content/stage-operations?limit=3",
            &read_headers,
            b"",
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(payload["entries"][2]["outcome"], "replayed");

        let response = request(
            &app,
            Method::GET,
            "/api/v1/content/stage-operations?limit=0",
            &read_headers,
            b"",
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response_json(response).await["error"], "invalid_pagination");
    }

    #[tokio::test]
    async fn content_rollback_route_requires_csrf_and_appends_audit() {
        let (_directory, state) = state();
        let baseline = state
            .store
            .active_content_revision()
            .expect("应读取 Web 回滚基线 revision");
        let target = effect_package("web-rollback-target", "web-rollback-effect", "entangle");
        state
            .store
            .stage_content_package(&target)
            .expect("应写入 Web 回滚目标草稿");
        assert!(
            state
                .store
                .validate_content_draft("web-rollback-target", 1)
                .expect("应校验 Web 回滚目标草稿")
                .errors
                .is_empty()
        );
        state
            .store
            .publish_content_draft("web-rollback-target", 1)
            .expect("应发布 Web 回滚目标 revision");
        let app = build_router(state.clone());
        let rollback_path = format!("/api/v1/content/revisions/{}/rollback", baseline.id);

        let response = request(&app, Method::POST, &rollback_path, &[], b"").await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let (cookie, csrf_token) = login_for_test(&app).await;
        let read_headers = [("cookie", cookie.as_str())];
        let write_headers = [
            ("cookie", cookie.as_str()),
            ("x-csrf-token", csrf_token.as_str()),
        ];
        let response = request(&app, Method::POST, &rollback_path, &read_headers, b"").await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let response = request(
            &app,
            Method::POST,
            "/api/v1/content/revisions/0/rollback",
            &write_headers,
            b"",
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response_json(response).await["error"],
            "invalid_revision_id"
        );

        let response = request(
            &app,
            Method::POST,
            "/api/v1/content/revisions/999999/rollback",
            &write_headers,
            b"",
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(response_json(response).await["error"], "not_found");

        let response = request(&app, Method::POST, &rollback_path, &write_headers, b"").await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let first = response_json(response).await;
        assert_eq!(first["revision"]["id"], baseline.id);
        assert_eq!(first["active_revision_id"], baseline.id);
        let first_activation_id = first["activation_id"]
            .as_i64()
            .expect("回滚响应应返回 activation 标识");

        let response = request(&app, Method::POST, &rollback_path, &write_headers, b"").await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let second = response_json(response).await;
        assert_ne!(second["activation_id"], first_activation_id);
        assert_eq!(
            state
                .store
                .active_content_revision()
                .expect("应读取 Web 回滚后的 active revision")
                .id,
            baseline.id
        );

        let response = request(
            &app,
            Method::GET,
            "/api/v1/content/rollback-operations",
            &[],
            b"",
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let response = request(
            &app,
            Method::GET,
            "/api/v1/content/rollback-operations?limit=2",
            &read_headers,
            b"",
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        let entries = payload["entries"].as_array().expect("应返回回滚审计列表");
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|entry| {
            entry["actor_role"] == "content_admin"
                && entry["revision_id"] == baseline.id
                && entry.get("actor_fingerprint").is_none()
        }));
        assert_eq!(entries[0]["activation_id"], first_activation_id);
        assert_eq!(entries[1]["activation_id"], second["activation_id"]);

        let response = request(
            &app,
            Method::GET,
            "/api/v1/content/rollback-operations?limit=0",
            &read_headers,
            b"",
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response_json(response).await["error"], "invalid_pagination");
    }

    #[tokio::test]
    async fn player_stage_routes_require_auth_csrf_and_confirm_one_candidate() {
        let (directory, state) = state();
        create_player_stage_file(directory.path());
        let app = build_router(state.clone());
        let candidates_path = "/api/v1/player-staging/candidates?stage_file=web-stage.sqlite";
        let confirm_path = "/api/v1/player-staging/confirm";

        let response = request(&app, Method::GET, candidates_path, &[], b"").await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let (cookie, csrf_token) = login_for_test(&app).await;
        let read_headers = [("cookie", cookie.as_str())];
        let write_headers = [
            ("cookie", cookie.as_str()),
            ("x-csrf-token", csrf_token.as_str()),
            ("content-type", "application/json"),
        ];
        let response = request(
            &app,
            Method::GET,
            "/api/v1/player-staging/candidates?stage_file=../web-stage.sqlite",
            &read_headers,
            b"",
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response_json(response).await["error"],
            "invalid_player_stage_file"
        );

        let response = request(&app, Method::GET, candidates_path, &read_headers, b"").await;
        assert_eq!(response.status(), StatusCode::OK);
        let candidates = response_json(response).await;
        assert_eq!(candidates["protocol"], "onebot11");
        assert_eq!(candidates["account_id"], "10001");
        assert_eq!(candidates["entries"][0]["source_player_id"], 1);
        assert_eq!(candidates["entries"][0]["subject_id"], "30001");
        assert_eq!(candidates["entries"][0]["name"], "Web确认角色");
        assert!(candidates.get("source_sha256").is_none());
        assert!(candidates["entries"][0].get("source_sha256").is_none());

        let response = request(
            &app,
            Method::POST,
            confirm_path,
            &[
                ("cookie", cookie.as_str()),
                ("content-type", "application/json"),
            ],
            br#"{"stage_file":"web-stage.sqlite","source_player_id":1}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(response_json(response).await["error"], "csrf_required");

        let response = request(
            &app,
            Method::POST,
            confirm_path,
            &write_headers,
            br#"{"stage_file":"web-stage.sqlite","source_player_id":1}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let receipt = response_json(response).await;
        assert_eq!(receipt["source_player_id"], 1);
        assert_eq!(receipt["name"], "Web确认角色");
        assert_eq!(receipt["level"], 1);
        assert_eq!(receipt["map_name"], "圣魂村");
        assert!(receipt.get("actor_fingerprint").is_none());

        let response = request(
            &app,
            Method::POST,
            confirm_path,
            &write_headers,
            br#"{"stage_file":"web-stage.sqlite","source_player_id":1}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            response_json(response).await["error"],
            "player_stage_conflict"
        );
        assert_eq!(
            state
                .store
                .player_status(&crate::store::IdentityKey {
                    protocol: crate::message::Protocol::OneBot11,
                    account_id: "10001",
                    namespace: "default",
                    subject_kind: "user",
                    subject_id: "30001",
                })
                .expect("应读取 HTTP 确认后的角色")
                .expect("HTTP 确认后角色必须存在")
                .name,
            "Web确认角色"
        );
    }

    #[tokio::test]
    async fn player_stage_legacy_identity_error_maps_to_conflict() {
        let response = player_stage_confirmation_error(LEGACY_CLAIM_REQUIRED);
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            response_json(response).await["error"],
            "player_stage_conflict"
        );
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
            ManagementServer::start(&web_config, state.store.clone(), directory.path())
                .expect("管理服务应启动");
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
