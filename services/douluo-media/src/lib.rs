//! Read-only media service used by the Douluo Bot.
//!
//! The service deliberately keeps its public surface small: an image directory is
//! indexed once at startup and is then exposed through an alias URL (the relative
//! `asset_key`) and a content-addressed URL.  There is no directory listing and no
//! request-controlled filesystem path.

mod catalog;

use std::{
    collections::{HashMap, HashSet},
    env, fmt,
    fs::{self, File, Metadata},
    io::{self, Read},
    net::SocketAddr,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    Router,
    body::{Body, Bytes},
    extract::{OriginalUri, Path as AxumPath, State},
    http::{
        StatusCode,
        header::{self, HeaderMap, HeaderName, HeaderValue},
    },
    response::{IntoResponse, Response},
    routing::get,
};
use sha2::{Digest, Sha256};
use tokio::{
    fs::File as TokioFile,
    io::AsyncReadExt,
    net::TcpListener,
    sync::{OwnedSemaphorePermit, Semaphore},
};

pub use catalog::CatalogError;

const DEFAULT_BIND: &str = "127.0.0.1:18182";
// Keep this in sync with the plugin's stable remote-asset-key contract.
const MAX_ASSET_KEY_LENGTH: usize = 200;
const HASH_LENGTH: usize = 64;
const HASH_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";
const ALIAS_CACHE_CONTROL: &str = "public, max-age=60, must-revalidate";
const MAX_CONCURRENT_RESPONSES: usize = 4;

/// Maximum file size accepted by the initial read-only service.
pub const MAX_ASSET_SIZE: u64 = 20 * 1024 * 1024;

/// Image extensions accepted by the indexer.
const ALLOWED_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "bmp"];
const IMAGE_MAGIC_HEADER_BYTES: u64 = 12;

/// 由 [`MediaConfig::from_env`] 读取的运行期配置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaConfig {
    /// 服务监听地址。
    pub bind: SocketAddr,
    /// 包含已发布媒体文件的目录。
    pub root: PathBuf,
    /// 只读发布 catalog；默认位于 published root 的 `catalog.sqlite`。
    pub catalog: PathBuf,
}

impl MediaConfig {
    /// 显式构造默认使用 root 内 catalog 的配置。
    pub fn new(bind: SocketAddr, root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            bind,
            catalog: root.join("catalog.sqlite"),
            root,
        }
    }

    /// 构造指定 catalog 路径的运行配置，供部署和集成测试使用。
    pub fn with_catalog(
        bind: SocketAddr,
        root: impl Into<PathBuf>,
        catalog: impl Into<PathBuf>,
    ) -> Self {
        Self {
            bind,
            root: root.into(),
            catalog: catalog.into(),
        }
    }

    /// 读取 `DOULUO_MEDIA_BIND`、`DOULUO_MEDIA_ROOT` 和可选 catalog 路径。
    ///
    /// 监听地址默认是 `127.0.0.1:18182`。root 必须显式配置，避免意外公开当前目录。
    pub fn from_env() -> Result<Self, MediaError> {
        let bind = match env::var("DOULUO_MEDIA_BIND") {
            Ok(value) if !value.trim().is_empty() => value,
            Ok(_) => return Err(MediaError::InvalidBind),
            Err(env::VarError::NotPresent) => DEFAULT_BIND.to_owned(),
            Err(env::VarError::NotUnicode(_)) => return Err(MediaError::InvalidBind),
        };
        let bind = bind.parse().map_err(|_| MediaError::InvalidBind)?;

        let root = env::var_os("DOULUO_MEDIA_ROOT").ok_or(MediaError::MissingRoot)?;
        if root.is_empty() {
            return Err(MediaError::MissingRoot);
        }
        let root = PathBuf::from(root);
        let catalog = match env::var_os("DOULUO_MEDIA_CATALOG") {
            Some(path) if path.is_empty() => return Err(MediaError::InvalidCatalog),
            Some(path) => PathBuf::from(path),
            None => root.join("catalog.sqlite"),
        };

        Ok(Self::with_catalog(bind, root, catalog))
    }
}

/// Errors which prevent the service from starting.
#[derive(Debug)]
pub enum MediaError {
    MissingRoot,
    InvalidBind,
    RootUnavailable,
    RootNotDirectory,
    IndexUnavailable,
    InvalidCatalog,
    CatalogUnavailable,
    BindUnavailable,
    ServerUnavailable,
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::MissingRoot => "DOULUO_MEDIA_ROOT is required",
            Self::InvalidBind => "DOULUO_MEDIA_BIND must be a valid socket address",
            Self::RootUnavailable => "media root is unavailable",
            Self::RootNotDirectory => "media root is not a directory",
            Self::IndexUnavailable => "media index could not be built",
            Self::InvalidCatalog => "DOULUO_MEDIA_CATALOG must not be empty",
            Self::CatalogUnavailable => "media catalog is unavailable or inconsistent",
            Self::BindUnavailable => "media listener could not bind",
            Self::ServerUnavailable => "media server stopped unexpectedly",
        };
        f.write_str(message)
    }
}

impl std::error::Error for MediaError {}

/// Metadata captured for one indexed image.
#[derive(Clone, Debug)]
pub struct AssetMeta {
    asset_key: String,
    path: PathBuf,
    sha256: String,
    mime: &'static str,
    extension: String,
    size: u64,
    modified: SystemTime,
}

impl AssetMeta {
    /// Stable URL key relative to the configured media root.
    pub fn asset_key(&self) -> &str {
        &self.asset_key
    }

    /// Lower-case SHA-256 digest of the file contents.
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// MIME type advertised by the service.
    pub fn mime(&self) -> &str {
        self.mime
    }

    /// Lower-case file extension used by the content-addressed URL.
    pub fn extension(&self) -> &str {
        &self.extension
    }

    /// File size captured during startup indexing.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Modification time captured during startup indexing.
    pub fn modified(&self) -> SystemTime {
        self.modified
    }
}

/// Immutable startup index.
#[derive(Clone, Debug)]
pub struct MediaIndex {
    root: PathBuf,
    by_key: HashMap<String, Arc<AssetMeta>>,
    by_hash_extension: HashMap<(String, String), Arc<AssetMeta>>,
}

impl MediaIndex {
    /// Recursively index allowed image files below `root`.
    pub fn build(root: impl AsRef<Path>) -> Result<Self, MediaError> {
        let root = fs::canonicalize(root).map_err(|_| MediaError::RootUnavailable)?;
        let root_metadata = fs::metadata(&root).map_err(|_| MediaError::RootUnavailable)?;
        if !root_metadata.is_dir() {
            return Err(MediaError::RootNotDirectory);
        }

        let mut index = Self {
            root,
            by_key: HashMap::new(),
            by_hash_extension: HashMap::new(),
        };
        let mut visited_directories = HashSet::new();
        index.walk_directory(index.root.clone(), &mut visited_directories)?;
        Ok(index)
    }

    /// Look up an alias key.
    pub fn get(&self, asset_key: &str) -> Option<&AssetMeta> {
        self.by_key.get(asset_key).map(Arc::as_ref)
    }

    /// Look up a content-addressed resource.
    pub fn get_by_hash(&self, sha256: &str, extension: &str) -> Option<&AssetMeta> {
        self.by_hash_extension
            .get(&(sha256.to_ascii_lowercase(), extension.to_ascii_lowercase()))
            .map(Arc::as_ref)
    }

    /// Number of indexed files.
    pub fn len(&self) -> usize {
        self.by_key.len()
    }

    /// Whether no files were indexed.
    pub fn is_empty(&self) -> bool {
        self.by_key.is_empty()
    }

    /// 按稳定资源键排序返回启动索引中的全部资源，供 catalog 发布和校验使用。
    pub(crate) fn assets(&self) -> Vec<&AssetMeta> {
        let mut assets = self.by_key.values().map(Arc::as_ref).collect::<Vec<_>>();
        assets.sort_by(|left, right| left.asset_key.cmp(&right.asset_key));
        assets
    }

    fn walk_directory(
        &mut self,
        directory: PathBuf,
        visited_directories: &mut HashSet<PathBuf>,
    ) -> Result<(), MediaError> {
        let canonical_directory = match fs::canonicalize(&directory) {
            Ok(path) if is_within_root(&self.root, &path) => path,
            // Broken or escaping symlinks are intentionally invisible.
            _ => return Ok(()),
        };
        if !visited_directories.insert(canonical_directory) {
            return Ok(());
        }

        let mut entries = fs::read_dir(&directory)
            .map_err(|_| MediaError::IndexUnavailable)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| MediaError::IndexUnavailable)?;
        entries.sort_by_key(|entry| entry.file_name());

        for entry in entries {
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };

            // Canonicalizing every entry prevents a symlink (including one in a
            // parent component) from escaping the configured root.
            let canonical_path = match fs::canonicalize(&path) {
                Ok(path) if is_within_root(&self.root, &path) => path,
                _ => continue,
            };
            let metadata = match fs::metadata(&path) {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };

            if metadata.is_dir() {
                self.walk_directory(path, visited_directories)?;
                continue;
            }
            if !metadata.is_file() || (!file_type.is_file() && !file_type.is_symlink()) {
                continue;
            }
            if metadata.len() > MAX_ASSET_SIZE {
                continue;
            }

            let Some(asset_key) = relative_asset_key(&self.root, &path) else {
                continue;
            };
            let Some(extension) = allowed_extension(&path) else {
                continue;
            };
            let Some(mime) = detect_image_mime(&canonical_path).ok().flatten() else {
                continue;
            };
            if mime_for_extension(&extension) != Some(mime) {
                continue;
            }

            let (sha256, size) = match hash_file(&canonical_path, &metadata) {
                Ok(value) => value,
                Err(_) => continue,
            };
            let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
            let asset = Arc::new(AssetMeta {
                asset_key: asset_key.clone(),
                path: canonical_path,
                sha256: sha256.clone(),
                mime,
                extension: extension.clone(),
                size,
                modified,
            });

            self.by_key.insert(asset_key, Arc::clone(&asset));
            self.by_hash_extension
                .entry((sha256, extension))
                .or_insert(asset);
        }

        Ok(())
    }

    fn root(&self) -> &Path {
        &self.root
    }
}

/// Shared application state used by the Axum router.
#[derive(Clone, Debug)]
pub struct MediaState {
    index: Arc<MediaIndex>,
    ready: bool,
    read_permits: Arc<Semaphore>,
}

impl MediaState {
    /// 仅按媒体 root 建立状态，供不使用 catalog 的嵌入测试保留。
    pub fn from_root(root: impl AsRef<Path>) -> Result<Self, MediaError> {
        Ok(Self {
            index: Arc::new(MediaIndex::build(root)?),
            ready: true,
            read_permits: Arc::new(Semaphore::new(MAX_CONCURRENT_RESPONSES)),
        })
    }

    /// 从只读 catalog 与发布根建立状态；任何键或文件元数据不一致都会拒绝启动。
    pub fn from_catalog(
        root: impl AsRef<Path>,
        catalog_path: impl AsRef<Path>,
    ) -> Result<Self, MediaError> {
        let index = MediaIndex::build(root)?;
        catalog::validate_published_catalog(&index, catalog_path)
            .map_err(|_| MediaError::CatalogUnavailable)?;
        Ok(Self::from_index(index))
    }

    /// 由预先构建的索引建立状态，供嵌入和测试使用。
    pub fn from_index(index: MediaIndex) -> Self {
        Self {
            index: Arc::new(index),
            ready: true,
            read_permits: Arc::new(Semaphore::new(MAX_CONCURRENT_RESPONSES)),
        }
    }

    /// 访问不可变启动索引。
    pub fn index(&self) -> &MediaIndex {
        &self.index
    }

    /// 启动索引是否已完成。
    pub fn is_ready(&self) -> bool {
        self.ready
    }
}

/// 构造只读媒体路由。
pub fn build_router(state: Arc<MediaState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz).head(healthz))
        .route("/readyz", get(readyz).head(readyz))
        // Keep the hash route before the alias wildcard for readability. Axum
        // chooses the more specific route regardless of declaration order.
        .route("/media/sha256/{*hash_asset}", get(hash_get).head(hash_head))
        .route("/media/{*asset_key}", get(alias_get).head(alias_head))
        .with_state(state)
}

/// 保留简短的嵌入式路由别名。
pub fn router(state: Arc<MediaState>) -> Router {
    build_router(state)
}

/// 使用环境变量启动服务。
pub async fn run_from_env() -> Result<(), MediaError> {
    let config = MediaConfig::from_env()?;
    run(config).await
}

/// 使用显式配置启动服务。
pub async fn run(config: MediaConfig) -> Result<(), MediaError> {
    let state = Arc::new(MediaState::from_catalog(&config.root, &config.catalog)?);
    let listener = TcpListener::bind(config.bind)
        .await
        .map_err(|_| MediaError::BindUnavailable)?;
    axum::serve(listener, build_router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|_| MediaError::ServerUnavailable)
}

/// 发布前把当前 published root 写入 SQLite catalog；运行期不调用此函数。
pub fn publish_catalog(
    root: impl AsRef<Path>,
    catalog_path: impl AsRef<Path>,
) -> Result<(), MediaError> {
    let index = MediaIndex::build(root)?;
    catalog::publish_catalog(&index, catalog_path).map_err(|_| MediaError::CatalogUnavailable)
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        else {
            let _ = tokio::signal::ctrl_c().await;
            return;
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn healthz() -> Response {
    health_response(StatusCode::OK, "ok\n")
}

async fn readyz(State(state): State<Arc<MediaState>>) -> Response {
    if state.is_ready() {
        health_response(StatusCode::OK, "ready\n")
    } else {
        health_response(StatusCode::SERVICE_UNAVAILABLE, "not ready\n")
    }
}

fn health_response(status: StatusCode, body: &'static str) -> Response {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    response
}

async fn alias_get(
    State(state): State<Arc<MediaState>>,
    AxumPath(asset_key): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if !is_safe_raw_media_path(uri.path()) {
        return HttpError::InvalidAssetKey.into_response();
    }
    serve_key(state, asset_key, headers, false).await
}

async fn alias_head(
    State(state): State<Arc<MediaState>>,
    AxumPath(asset_key): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if !is_safe_raw_media_path(uri.path()) {
        return HttpError::InvalidAssetKey.into_response();
    }
    serve_key(state, asset_key, headers, true).await
}

async fn hash_get(
    State(state): State<Arc<MediaState>>,
    AxumPath(hash_asset): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if !is_safe_raw_media_path(uri.path()) {
        return HttpError::InvalidAssetKey.into_response();
    }
    serve_hash(state, hash_asset, headers, false).await
}

async fn hash_head(
    State(state): State<Arc<MediaState>>,
    AxumPath(hash_asset): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if !is_safe_raw_media_path(uri.path()) {
        return HttpError::InvalidAssetKey.into_response();
    }
    serve_hash(state, hash_asset, headers, true).await
}

async fn serve_key(
    state: Arc<MediaState>,
    asset_key: String,
    headers: HeaderMap,
    head: bool,
) -> Response {
    if !is_safe_asset_key(&asset_key) {
        return HttpError::InvalidAssetKey.into_response();
    }
    let Some(asset) = state.index.get(&asset_key) else {
        return HttpError::NotFound.into_response();
    };
    serve_asset(&state, asset, headers, head, false).await
}

async fn serve_hash(
    state: Arc<MediaState>,
    hash_asset: String,
    headers: HeaderMap,
    head: bool,
) -> Response {
    let Some((digest, extension)) = parse_hash_path(&hash_asset) else {
        return HttpError::NotFound.into_response();
    };
    let Some(asset) = state.index.get_by_hash(&digest, &extension) else {
        return HttpError::NotFound.into_response();
    };
    serve_asset(&state, asset, headers, head, true).await
}

async fn serve_asset(
    state: &MediaState,
    asset: &AssetMeta,
    request_headers: HeaderMap,
    head: bool,
    content_addressed: bool,
) -> Response {
    let (mut bytes, permit) = match read_verified_asset(state, asset).await {
        Ok(value) => value,
        Err(ReadAssetError::Busy) => return HttpError::Busy.into_response(),
        Err(ReadAssetError::Unavailable) => return HttpError::NotFound.into_response(),
    };

    let mut response_headers = asset_headers(asset, content_addressed);
    let has_if_none_match = request_headers.get(header::IF_NONE_MATCH).is_some();
    if if_none_match_matches(request_headers.get(header::IF_NONE_MATCH), &asset.sha256)
        || (!has_if_none_match
            && if_modified_since_matches(
                request_headers.get(header::IF_MODIFIED_SINCE),
                asset.modified,
            ))
    {
        // RFC 9110 permits Content-Length on 304 when it describes the selected
        // 200 representation. Setting it also prevents Axum from inserting 0.
        response_headers.insert(
            header::CONTENT_LENGTH,
            header_value(&asset.size.to_string()),
        );
        return response_from_parts(StatusCode::NOT_MODIFIED, response_headers, Body::empty());
    }

    let range_header = if !head && if_range_matches(request_headers.get(header::IF_RANGE), asset) {
        request_headers.get(header::RANGE)
    } else {
        None
    };
    let requested_range = match range_header.map(|value| {
        value
            .to_str()
            .map_err(|_| RangeError::Invalid)
            .and_then(|value| parse_range(value, asset.size))
    }) {
        Some(Ok(range)) => range,
        Some(Err(_)) => {
            return range_not_satisfiable(asset.size, head);
        }
        None => None,
    };

    let (status, start, end) = match requested_range {
        Some(range) => {
            response_headers.insert(
                header::CONTENT_RANGE,
                header_value(&format!(
                    "bytes {}-{}/{}",
                    range.start, range.end, asset.size
                )),
            );
            (StatusCode::PARTIAL_CONTENT, range.start, range.end)
        }
        None => {
            let end = asset.size.saturating_sub(1);
            (StatusCode::OK, 0, end)
        }
    };
    let content_length = if asset.size == 0 {
        0
    } else {
        end.saturating_sub(start).saturating_add(1)
    };
    response_headers.insert(
        header::CONTENT_LENGTH,
        header_value(&content_length.to_string()),
    );

    if head || content_length == 0 {
        return response_from_parts(status, response_headers, Body::empty());
    }

    let start = usize::try_from(start).expect("indexed assets fit in usize");
    let length = usize::try_from(content_length).expect("indexed assets fit in usize");
    if start != 0 {
        bytes.copy_within(start..start + length, 0);
    }
    if length != bytes.len() {
        bytes.truncate(length);
        bytes.shrink_to_fit();
    }
    response_from_parts(
        status,
        response_headers,
        Body::from(Bytes::from_owner(PermittedBytes {
            bytes,
            _permit: permit,
        })),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReadAssetError {
    Busy,
    Unavailable,
}

async fn read_verified_asset(
    state: &MediaState,
    asset: &AssetMeta,
) -> Result<(Vec<u8>, OwnedSemaphorePermit), ReadAssetError> {
    let permit = Arc::clone(&state.read_permits)
        .try_acquire_owned()
        .map_err(|_| ReadAssetError::Busy)?;
    let path = safe_indexed_path(state.index(), asset).ok_or(ReadAssetError::Unavailable)?;
    let mut file = TokioFile::open(path)
        .await
        .map_err(|_| ReadAssetError::Unavailable)?;
    let mut bytes = Vec::with_capacity(asset.size as usize);
    (&mut file)
        .take(MAX_ASSET_SIZE + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| ReadAssetError::Unavailable)?;
    let metadata = file
        .metadata()
        .await
        .map_err(|_| ReadAssetError::Unavailable)?;
    if !metadata.is_file()
        || bytes.len() as u64 != asset.size
        || metadata.len() != asset.size
        || metadata.modified().unwrap_or(UNIX_EPOCH) != asset.modified
        || sha256_hex(&bytes) != asset.sha256
    {
        return Err(ReadAssetError::Unavailable);
    }
    Ok((bytes, permit))
}

struct PermittedBytes {
    bytes: Vec<u8>,
    _permit: OwnedSemaphorePermit,
}

impl AsRef<[u8]> for PermittedBytes {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

fn response_from_parts(status: StatusCode, headers: HeaderMap, body: Body) -> Response {
    let mut response = Response::new(body);
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    response
}

fn range_not_satisfiable(size: u64, head: bool) -> Response {
    let body = b"range not satisfiable\n";
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    headers.insert(
        header::CONTENT_RANGE,
        header_value(&format!("bytes */{size}")),
    );
    headers.insert(
        header::CONTENT_LENGTH,
        header_value(&body.len().to_string()),
    );
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    response_from_parts(
        StatusCode::RANGE_NOT_SATISFIABLE,
        headers,
        if head {
            Body::empty()
        } else {
            Body::from(body.to_vec())
        },
    )
}

fn asset_headers(asset: &AssetMeta, content_addressed: bool) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, header_value(asset.mime));
    headers.insert(header::ETAG, header_value(&format!("\"{}\"", asset.sha256)));
    headers.insert(
        header::LAST_MODIFIED,
        header_value(&httpdate::fmt_http_date(asset.modified)),
    );
    headers.insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(if content_addressed {
            HASH_CACHE_CONTROL
        } else {
            ALIAS_CACHE_CONTROL
        }),
    );
    headers
}

fn header_value(value: &str) -> HeaderValue {
    // All values passed here are generated from validated metadata or fixed
    // protocol tokens.  Falling back to an empty value keeps an unexpected
    // platform timestamp from becoming a request failure.
    HeaderValue::try_from(value).unwrap_or_else(|_| HeaderValue::from_static(""))
}

fn if_none_match_matches(value: Option<&HeaderValue>, digest: &str) -> bool {
    let Some(value) = value.and_then(|value| value.to_str().ok()) else {
        return false;
    };
    value.split(',').map(str::trim).any(|tag| {
        tag == "*" || tag.strip_prefix("W/").unwrap_or(tag).trim() == format!("\"{digest}\"")
    })
}

fn if_range_matches(value: Option<&HeaderValue>, asset: &AssetMeta) -> bool {
    let Some(value) = value.and_then(|value| value.to_str().ok()) else {
        return value.is_none();
    };
    let value = value.trim();
    if value.starts_with('"') {
        return value == format!("\"{}\"", asset.sha256);
    }
    if value.starts_with("W/") {
        return false;
    }
    httpdate::parse_http_date(value)
        .map(|date| truncate_to_http_second(asset.modified) <= date)
        .unwrap_or(false)
}

fn parse_hash_path(value: &str) -> Option<(String, String)> {
    let (digest, extension) = value.rsplit_once('.')?;
    if digest.len() != HASH_LENGTH
        || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        || digest.contains('/')
        || extension.is_empty()
        || extension.contains('/')
    {
        return None;
    }
    let extension = extension.to_ascii_lowercase();
    if !ALLOWED_EXTENSIONS.contains(&extension.as_str()) {
        return None;
    }
    Some((digest.to_ascii_lowercase(), extension))
}

/// Validate an asset key before using it as a URL alias.
pub fn is_safe_asset_key(value: &str) -> bool {
    if value.is_empty()
        || value.len() > MAX_ASSET_KEY_LENGTH
        || value.starts_with('/')
        || value.ends_with('/')
        || value.contains("//")
        || value.contains('\\')
        || value.contains('\0')
    {
        return false;
    }

    let mut segments = value.split('/');
    let Some(first) = segments.next() else {
        return false;
    };
    // Reserved by `/media/sha256/{digest}.{ext}`.
    if first == "sha256" {
        return false;
    }
    for segment in std::iter::once(first).chain(segments) {
        if segment.is_empty() || segment == "." || segment == ".." {
            return false;
        }
        if !segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return false;
        }
    }
    true
}

fn relative_asset_key(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let mut segments = Vec::new();
    for component in relative.components() {
        let Component::Normal(segment) = component else {
            return None;
        };
        segments.push(segment.to_str()?);
    }
    let key = segments.join("/");
    is_safe_asset_key(&key).then_some(key)
}

fn allowed_extension(path: &Path) -> Option<String> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    ALLOWED_EXTENSIONS
        .contains(&extension.as_str())
        .then_some(extension)
}

fn mime_for_extension(extension: &str) -> Option<&'static str> {
    Some(match extension {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        _ => return None,
    })
}

fn detect_image_mime(path: &Path) -> io::Result<Option<&'static str>> {
    let mut file = File::open(path)?;
    let mut header = Vec::with_capacity(IMAGE_MAGIC_HEADER_BYTES as usize);
    file.by_ref()
        .take(IMAGE_MAGIC_HEADER_BYTES)
        .read_to_end(&mut header)?;
    Ok(detect_image_mime_bytes(&header))
}

/// 根据实际文件头识别 MIME，扩展名不能参与该判断。
fn detect_image_mime_bytes(header: &[u8]) -> Option<&'static str> {
    if header.len() >= 8 && header[..8] == [137, 80, 78, 71, 13, 10, 26, 10] {
        Some("image/png")
    } else if header.len() >= 3 && header[..3] == [0xff, 0xd8, 0xff] {
        Some("image/jpeg")
    } else if header.len() >= 12 && &header[..4] == b"RIFF" && &header[8..12] == b"WEBP" {
        Some("image/webp")
    } else if header.len() >= 6 && (&header[..6] == b"GIF87a" || &header[..6] == b"GIF89a") {
        Some("image/gif")
    } else if header.len() >= 2 && &header[..2] == b"BM" {
        Some("image/bmp")
    } else {
        None
    }
}

fn hash_file(path: &Path, metadata: &Metadata) -> io::Result<(String, u64)> {
    if metadata.len() > MAX_ASSET_SIZE {
        return Err(io::Error::other("file exceeds media size limit"));
    }
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut size = 0_u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size = size.saturating_add(read as u64);
    }
    // Metadata size is retained as a consistency check. A file changing while
    // it is indexed is skipped instead of publishing a misleading digest.
    if size != metadata.len() {
        return Err(io::Error::other("file changed while indexing"));
    }
    let digest = digest_hex(hasher.finalize());
    Ok((digest, size))
}

fn sha256_hex(bytes: &[u8]) -> String {
    digest_hex(Sha256::digest(bytes))
}

fn digest_hex(digest: impl IntoIterator<Item = u8>) -> String {
    let mut text = String::with_capacity(HASH_LENGTH);
    for byte in digest {
        use fmt::Write as _;
        let _ = write!(text, "{byte:02x}");
    }
    text
}

fn is_within_root(root: &Path, candidate: &Path) -> bool {
    candidate == root || candidate.starts_with(root)
}

fn safe_indexed_path(index: &MediaIndex, asset: &AssetMeta) -> Option<PathBuf> {
    let canonical = fs::canonicalize(&asset.path).ok()?;
    if !is_within_root(index.root(), &canonical) {
        return None;
    }
    let metadata = fs::metadata(&canonical).ok()?;
    (metadata.is_file()
        && metadata.len() == asset.size
        && metadata.modified().unwrap_or(UNIX_EPOCH) == asset.modified)
        .then_some(canonical)
}

fn is_safe_raw_media_path(path: &str) -> bool {
    let Some(path) = path.strip_prefix("/media/") else {
        return false;
    };
    // Asset keys are deliberately ASCII and contain no percent sign. Checking
    // the raw URI as well as the decoded Path extractor closes encoded slash,
    // backslash, dot-segment, and malformed-percent variants.
    !path.contains('%')
}

fn if_modified_since_matches(value: Option<&HeaderValue>, modified: SystemTime) -> bool {
    let Some(value) = value.and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let Ok(since) = httpdate::parse_http_date(value) else {
        return false;
    };
    truncate_to_http_second(modified) <= since
}

fn truncate_to_http_second(value: SystemTime) -> SystemTime {
    value
        .duration_since(UNIX_EPOCH)
        .map(|duration| UNIX_EPOCH + Duration::from_secs(duration.as_secs()))
        .unwrap_or(UNIX_EPOCH)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ByteRange {
    start: u64,
    end: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RangeError {
    Invalid,
    Unsatisfiable,
}

fn parse_range(value: &str, size: u64) -> Result<Option<ByteRange>, RangeError> {
    let Some(value) = value.strip_prefix("bytes=") else {
        return Err(RangeError::Invalid);
    };
    // A single range is intentional. Multipart responses are outside this
    // service's contract and are rejected with 416.
    if value.contains(',') {
        return Err(RangeError::Invalid);
    }
    let (start, end) = value.split_once('-').ok_or(RangeError::Invalid)?;
    if start.is_empty() && end.is_empty() {
        return Err(RangeError::Invalid);
    }
    if size == 0 {
        return Err(RangeError::Unsatisfiable);
    }

    if start.is_empty() {
        let suffix = end.parse::<u64>().map_err(|_| RangeError::Invalid)?;
        if suffix == 0 {
            return Err(RangeError::Unsatisfiable);
        }
        let length = suffix.min(size);
        return Ok(Some(ByteRange {
            start: size - length,
            end: size - 1,
        }));
    }

    let start = start.parse::<u64>().map_err(|_| RangeError::Invalid)?;
    if start >= size {
        return Err(RangeError::Unsatisfiable);
    }
    let end = if end.is_empty() {
        size - 1
    } else {
        let end = end.parse::<u64>().map_err(|_| RangeError::Invalid)?;
        if end < start {
            return Err(RangeError::Unsatisfiable);
        }
        end.min(size - 1)
    };
    Ok(Some(ByteRange { start, end }))
}

#[derive(Clone, Copy, Debug)]
enum HttpError {
    InvalidAssetKey,
    NotFound,
    Busy,
}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            Self::InvalidAssetKey => (StatusCode::BAD_REQUEST, "invalid asset key\n"),
            Self::NotFound => (StatusCode::NOT_FOUND, "asset not found\n"),
            Self::Busy => (StatusCode::SERVICE_UNAVAILABLE, "service busy\n"),
        };
        let mut response = Response::new(Body::from(body));
        *response.status_mut() = status;
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        headers.insert(
            HeaderName::from_static("x-content-type-options"),
            HeaderValue::from_static("nosniff"),
        );
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::to_bytes,
        http::{Method, Request},
    };
    use tempfile::TempDir;
    use tower::ServiceExt;

    fn fixture() -> (TempDir, Arc<MediaState>, String) {
        let directory = tempfile::tempdir().expect("tempdir");
        let image = directory.path().join("maps").join("village.png");
        fs::create_dir_all(image.parent().expect("parent")).expect("mkdir");
        fs::write(&image, b"\x89PNG\r\n\x1a\n0123456789").expect("fixture");
        fs::write(directory.path().join("ignore.txt"), b"ignore").expect("fixture");
        fs::write(directory.path().join("fake.png"), b"not an image").expect("fixture");
        fs::write(directory.path().join("mismatch.jpg"), b"\x89PNG\r\n\x1a\n").expect("fixture");
        fs::write(
            directory.path().join("webp-disguised-as-jpeg.jpg"),
            b"RIFF\x04\x00\x00\x00WEBP",
        )
        .expect("fixture");
        let index = MediaIndex::build(directory.path()).expect("index");
        let digest = index.get("maps/village.png").expect("asset").sha256.clone();
        (directory, Arc::new(MediaState::from_index(index)), digest)
    }

    async fn request(
        app: &Router,
        method: Method,
        uri: &str,
        headers: &[(&str, &str)],
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        let mut builder = Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let response = app
            .clone()
            .oneshot(builder.body(Body::empty()).expect("request"))
            .await
            .expect("response");
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("body")
            .to_vec();
        (status, headers, body)
    }

    #[tokio::test]
    async fn serves_alias_and_sets_security_headers() {
        let (_directory, state, _digest) = fixture();
        let app = build_router(state.clone());
        let (status, headers, body) =
            request(&app, Method::GET, "/media/maps/village.png", &[]).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, b"\x89PNG\r\n\x1a\n0123456789");
        assert_eq!(headers.get(header::CONTENT_TYPE).unwrap(), "image/png");
        assert_eq!(headers.get(header::ACCEPT_RANGES).unwrap(), "bytes");
        assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
        assert_eq!(
            headers.get(header::CACHE_CONTROL).unwrap(),
            ALIAS_CACHE_CONTROL
        );
        assert!(headers.get(header::ETAG).is_some());
        assert!(headers.get(header::LAST_MODIFIED).is_some());
    }

    #[tokio::test]
    async fn head_has_get_headers_without_body() {
        let (_directory, state, _digest) = fixture();
        let app = build_router(state);
        let (status, headers, body) =
            request(&app, Method::HEAD, "/media/maps/village.png", &[]).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.is_empty());
        assert_eq!(headers.get(header::CONTENT_LENGTH).unwrap(), "18");

        let (status, headers, body) = request(
            &app,
            Method::HEAD,
            "/media/maps/village.png",
            &[("range", "bytes=8-11")],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.is_empty());
        assert_eq!(headers.get(header::CONTENT_LENGTH).unwrap(), "18");
        assert!(headers.get(header::CONTENT_RANGE).is_none());
    }

    #[tokio::test]
    async fn conditional_request_returns_not_modified() {
        let (_directory, state, _digest) = fixture();
        let etag = format!(
            "\"{}\"",
            state.index().get("maps/village.png").unwrap().sha256()
        );
        let app = build_router(state);
        let (status, _headers, body) = request(
            &app,
            Method::GET,
            "/media/maps/village.png",
            &[("if-none-match", &etag)],
        )
        .await;
        assert_eq!(status, StatusCode::NOT_MODIFIED);
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn serves_single_byte_range_and_rejects_invalid_range() {
        let (_directory, state, _digest) = fixture();
        let app = build_router(state);
        let (status, headers, body) = request(
            &app,
            Method::GET,
            "/media/maps/village.png",
            &[("range", "bytes=8-11")],
        )
        .await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(body, b"0123");
        assert_eq!(headers.get(header::CONTENT_RANGE).unwrap(), "bytes 8-11/18");
        assert_eq!(headers.get(header::CONTENT_LENGTH).unwrap(), "4");

        let (status, headers, body) = request(
            &app,
            Method::GET,
            "/media/maps/village.png",
            &[("range", "bytes=99-")],
        )
        .await;
        assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(headers.get(header::CONTENT_RANGE).unwrap(), "bytes */18");
        assert_eq!(headers.get(header::CACHE_CONTROL).unwrap(), "no-store");
        assert_eq!(
            headers.get(header::CONTENT_TYPE).unwrap(),
            "text/plain; charset=utf-8"
        );
        assert!(!body.is_empty());

        let (status, headers, body) = request(
            &app,
            Method::GET,
            "/media/maps/village.png",
            &[("range", "bytes=8-11"), ("if-range", "\"stale\"")],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers.get(header::CONTENT_LENGTH).unwrap(), "18");
        assert_eq!(body, b"\x89PNG\r\n\x1a\n0123456789");
    }

    #[tokio::test]
    async fn serves_content_addressed_url() {
        let (_directory, state, digest) = fixture();
        let app = build_router(state);
        let uri = format!("/media/sha256/{digest}.PNG");
        let (status, headers, body) = request(&app, Method::GET, &uri, &[]).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, b"\x89PNG\r\n\x1a\n0123456789");
        assert_eq!(
            headers.get(header::CACHE_CONTROL).unwrap(),
            HASH_CACHE_CONTROL
        );
    }

    #[tokio::test]
    async fn health_ready_and_missing_paths_are_safe() {
        let (_directory, state, _digest) = fixture();
        let app = build_router(state);
        let (status, _headers, body) = request(&app, Method::GET, "/healthz", &[]).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, b"ok\n");
        let (status, _headers, body) = request(&app, Method::HEAD, "/readyz", &[]).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.is_empty());

        let (status, _headers, body) = request(&app, Method::GET, "/media/nope.png", &[]).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let text = String::from_utf8(body).expect("utf8");
        assert!(!text.contains("douluo"));
        assert!(!text.contains("\\"));
    }

    #[test]
    fn catalog_backed_state_refuses_files_changed_outside_publish_command() {
        let directory = tempfile::tempdir().expect("tempdir");
        let image = directory.path().join("maps").join("village.png");
        fs::create_dir_all(image.parent().expect("parent")).expect("mkdir");
        fs::write(&image, b"\x89PNG\r\n\x1a\nrelease-one").expect("fixture");
        let catalog = directory.path().join("catalog.sqlite");

        publish_catalog(directory.path(), &catalog).expect("publish catalog");
        assert!(MediaState::from_catalog(directory.path(), &catalog).is_ok());

        fs::write(&image, b"\x89PNG\r\n\x1a\nrelease-two").expect("replace fixture");
        assert!(matches!(
            MediaState::from_catalog(directory.path(), &catalog),
            Err(MediaError::CatalogUnavailable)
        ));
    }

    #[tokio::test]
    async fn rejects_traversal_and_non_image_files() {
        let (_directory, state, _digest) = fixture();
        let app = build_router(state.clone());
        let (status, _headers, _body) =
            request(&app, Method::GET, "/media/%2e%2e/ignore.txt", &[]).await;
        assert!(status == StatusCode::BAD_REQUEST || status == StatusCode::NOT_FOUND);
        assert!(is_safe_asset_key("maps/village.png"));
        assert!(!is_safe_asset_key("../ignore.txt"));
        assert!(!is_safe_asset_key("maps\\village.png"));
        assert!(!is_safe_asset_key("maps/./village.png"));
        assert!(!is_safe_asset_key("maps//village.png"));
        assert!(!is_safe_asset_key("maps/%2e%2e/village.png"));
        assert!(!is_safe_asset_key("地图/village.png"));
        assert!(!is_safe_asset_key("sha256/alias.png"));
        assert!(is_safe_asset_key("maps/v1..2.png"));
        assert!(!is_safe_asset_key(&format!("{}.png", "a".repeat(201))));
        assert!(state.index().get("ignore.txt").is_none());
        assert!(state.index().get("fake.png").is_none());
        assert!(state.index().get("mismatch.jpg").is_none());
        assert!(state.index().get("webp-disguised-as-jpeg.jpg").is_none());
    }

    #[test]
    fn detects_webp_from_bytes_without_trusting_the_jpeg_extension() {
        let webp = b"RIFF\x04\x00\x00\x00WEBP";
        assert_eq!(detect_image_mime_bytes(webp), Some("image/webp"));
        assert_ne!(mime_for_extension("jpg"), detect_image_mime_bytes(webp));
    }

    #[tokio::test]
    async fn supports_if_modified_since_and_rejects_encoded_keys() {
        let (_directory, state, _digest) = fixture();
        let app = build_router(state);
        let (_status, headers, _body) =
            request(&app, Method::GET, "/media/maps/village.png", &[]).await;
        let last_modified = headers
            .get(header::LAST_MODIFIED)
            .expect("last modified")
            .to_str()
            .expect("header text")
            .to_owned();
        let (status, headers, body) = request(
            &app,
            Method::GET,
            "/media/maps/village.png",
            &[("if-modified-since", &last_modified)],
        )
        .await;
        assert_eq!(status, StatusCode::NOT_MODIFIED);
        assert!(body.is_empty());
        assert_eq!(headers.get(header::CONTENT_LENGTH).unwrap(), "18");

        let (status, _headers, _body) =
            request(&app, Method::GET, "/media/maps/%2e%2e/village.png", &[]).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _headers, _body) =
            request(&app, Method::GET, "/media/maps%2fvillage.png", &[]).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn skips_oversized_files_and_reserved_hash_prefix() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut bytes = vec![0_u8; (MAX_ASSET_SIZE + 1) as usize];
        bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        fs::write(directory.path().join("too-large.png"), bytes).expect("fixture");
        fs::create_dir_all(directory.path().join("sha256")).expect("mkdir");
        fs::write(
            directory.path().join("sha256").join("alias.png"),
            b"\x89PNG\r\n\x1a\n",
        )
        .expect("fixture");
        let index = MediaIndex::build(directory.path()).expect("index");
        assert!(index.is_empty());
    }

    #[tokio::test]
    async fn refuses_files_changed_after_startup_indexing() {
        let (directory, state, digest) = fixture();
        let app = build_router(state);
        let path = directory.path().join("maps").join("village.png");
        let original_modified = fs::metadata(&path)
            .expect("fixture metadata")
            .modified()
            .expect("fixture mtime");
        fs::write(&path, b"\x89PNG\r\n\x1a\n9876543210").expect("replace fixture");
        File::options()
            .write(true)
            .open(&path)
            .expect("open fixture")
            .set_times(fs::FileTimes::new().set_modified(original_modified))
            .expect("restore fixture mtime");

        let (status, _headers, _body) =
            request(&app, Method::GET, "/media/maps/village.png", &[]).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let uri = format!("/media/sha256/{digest}.png");
        let (status, _headers, _body) = request(&app, Method::GET, &uri, &[]).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn bounds_buffered_responses_and_releases_capacity_on_drop() {
        let (_directory, state, _digest) = fixture();
        let app = build_router(state);
        let mut held = Vec::new();
        for _ in 0..MAX_CONCURRENT_RESPONSES {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/media/maps/village.png")
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::OK);
            held.push(response);
        }

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/media/maps/village.png")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        drop(held.pop());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/media/maps/village.png")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
    }
}
