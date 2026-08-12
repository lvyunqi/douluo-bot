use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path::{Component, Path};

use serde::Deserialize;
use url::{Host, Url};

/// 插件启动配置。所有字段都提供无配置文件时的安全默认值。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct PluginConfig {
    pub database: DatabaseConfig,
    pub content: ContentConfig,
    pub web: WebConfig,
    pub identity: IdentityConfig,
    pub authorization: AuthorizationConfig,
    pub illustrations: IllustrationConfig,
    pub messages: MessageConfig,
}

/// 内容包文件导入与启动时发布开关。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ContentConfig {
    pub package_file: String,
    pub auto_publish: bool,
}

/// 管理服务的监听、远程暴露与认证启动配置。
#[derive(Clone, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct WebConfig {
    pub enabled: bool,
    pub bind: String,
    pub port: u16,
    pub allow_remote: bool,
    pub public_base_url: String,
    pub admin_secret: String,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: "127.0.0.1".to_string(),
            port: 18_181,
            allow_remote: false,
            public_base_url: String::new(),
            admin_secret: String::new(),
        }
    }
}

impl fmt::Debug for WebConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let secret = if self.admin_secret.is_empty() {
            "<empty>"
        } else {
            "<redacted>"
        };
        formatter
            .debug_struct("WebConfig")
            .field("enabled", &self.enabled)
            .field("bind", &self.bind)
            .field("port", &self.port)
            .field("allow_remote", &self.allow_remote)
            .field("public_base_url", &self.public_base_url)
            .field("admin_secret", &secret)
            .finish()
    }
}

impl WebConfig {
    /// 将受校验的绑定配置转换为实际监听地址。
    pub(crate) fn socket_addr(&self) -> Result<SocketAddr, String> {
        let ip = self
            .bind
            .parse::<IpAddr>()
            .map_err(|_| "web.bind 必须是合法 IP 地址".to_string())?;
        Ok(SocketAddr::new(ip, self.port))
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct DatabaseConfig {
    pub relative_path: String,
    pub busy_timeout_ms: u64,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            relative_path: "douluo-game/douluo.db".to_string(),
            busy_timeout_ms: 3_000,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct IdentityConfig {
    pub namespace: String,
    pub qq_official_account_id: String,
    pub max_character_name_chars: usize,
}

impl Default for IdentityConfig {
    fn default() -> Self {
        Self {
            namespace: "default".to_string(),
            qq_official_account_id: String::new(),
            max_character_name_chars: 6,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthorizationMode {
    #[default]
    AllowAll,
    Allowlist,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct AuthorizationConfig {
    pub mode: AuthorizationMode,
}

impl Default for AuthorizationConfig {
    fn default() -> Self {
        Self {
            mode: AuthorizationMode::AllowAll,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct MessageConfig {
    pub qq_official_markdown: bool,
    pub onebot_markdown: bool,
    pub legacy_hyphen_arguments: bool,
}

impl Default for MessageConfig {
    fn default() -> Self {
        Self {
            qq_official_markdown: true,
            onebot_markdown: false,
            legacy_hyphen_arguments: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IllustrationMode {
    #[default]
    Direct,
    Remote,
}

/// 插图投递配置：直连模式使用可信 URL 或 data_dir 内图片，远程模式使用配套图片服务。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct IllustrationConfig {
    pub enabled: bool,
    pub mode: IllustrationMode,
    pub direct_asset_root: String,
    pub remote_base_url: String,
}

impl Default for IllustrationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: IllustrationMode::Direct,
            direct_asset_root: "douluo-game/assets".to_string(),
            remote_base_url: String::new(),
        }
    }
}

impl IllustrationConfig {
    pub fn remote_asset_url(&self, asset_key: &str) -> Option<String> {
        let base_url = self.remote_base_url.trim_end_matches('/');
        (self.enabled
            && self.mode == IllustrationMode::Remote
            && is_safe_asset_key(asset_key)
            && validate_public_https_url(base_url, false).is_ok())
        .then(|| format!("{base_url}/media/{asset_key}"))
    }
}

/// 解析并执行业务级校验；JSON Schema 校验仍由 QimenBot 宿主负责。
pub fn parse_config(config_json: &str) -> Result<PluginConfig, String> {
    let config = if config_json.trim().is_empty() {
        PluginConfig::default()
    } else {
        serde_json::from_str(config_json).map_err(|error| format!("配置 JSON 无效：{error}"))?
    };
    validate_config(&config)?;
    Ok(config)
}

pub fn validate_config(config: &PluginConfig) -> Result<(), String> {
    if !is_safe_data_relative_path(&config.database.relative_path) {
        return Err("database.relative_path 必须是 data_dir 内的安全相对路径".to_string());
    }
    if !(100..=30_000).contains(&config.database.busy_timeout_ms) {
        return Err("database.busy_timeout_ms 必须在 100 到 30000 之间".to_string());
    }
    if !config.content.package_file.is_empty() {
        if !is_safe_data_relative_path(&config.content.package_file) {
            return Err("content.package_file 必须是 data_dir 内的安全相对路径".to_string());
        }
        let extension = Path::new(&config.content.package_file)
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase());
        if !matches!(extension.as_deref(), Some("json" | "toml")) {
            return Err("content.package_file 必须使用 .json 或 .toml 扩展名".to_string());
        }
    } else if config.content.auto_publish {
        return Err("content.auto_publish=true 时必须配置 package_file".to_string());
    }
    let web_bind = config
        .web
        .bind
        .parse::<IpAddr>()
        .map_err(|_| "web.bind 必须是合法 IP 地址".to_string())?;
    if config.web.port < 1024 {
        return Err("web.port 必须在 1024 到 65535 之间".to_string());
    }
    if !config.web.public_base_url.is_empty() {
        validate_public_https_url(&config.web.public_base_url, false)
            .map_err(|_| "web.public_base_url 必须是公网 HTTPS 地址".to_string())?;
    }
    if config.web.enabled {
        if !web_bind.is_loopback() && !config.web.allow_remote {
            return Err(
                "web.bind 仅允许回环地址；远程监听必须显式设置 web.allow_remote=true".to_string(),
            );
        }
        if !valid_web_admin_secret(&config.web.admin_secret) {
            return Err(
                "web.admin_secret 必须是无首尾空白和控制字符的 16-256 字符密钥".to_string(),
            );
        }
        if config.web.allow_remote && config.web.public_base_url.is_empty() {
            return Err("web.allow_remote=true 时必须配置 web.public_base_url".to_string());
        }
    }
    if !valid_namespace(&config.identity.namespace) {
        return Err(
            "identity.namespace 只能包含字母、数字、点、下划线和横线，长度 1-64".to_string(),
        );
    }
    if !config.identity.qq_official_account_id.is_empty()
        && !valid_identity_id(&config.identity.qq_official_account_id)
    {
        return Err(
            "identity.qq_official_account_id 必须是无首尾空白和控制字符的 1-128 字符字符串"
                .to_string(),
        );
    }
    if !(2..=20).contains(&config.identity.max_character_name_chars) {
        return Err("identity.max_character_name_chars 必须在 2 到 20 之间".to_string());
    }
    if !config.illustrations.direct_asset_root.is_empty()
        && !is_safe_data_relative_path(&config.illustrations.direct_asset_root)
    {
        return Err("illustrations.direct_asset_root 必须是 data_dir 内的安全相对路径".to_string());
    }
    if config.illustrations.mode == IllustrationMode::Remote {
        validate_remote_base_url(&config.illustrations.remote_base_url)?;
    }
    Ok(())
}

fn valid_namespace(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn valid_identity_id(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= 128
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn valid_web_admin_secret(value: &str) -> bool {
    (16..=256).contains(&value.chars().count())
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

pub(crate) fn is_safe_asset_key(value: &str) -> bool {
    if value.is_empty()
        || value.len() > 200
        || value.starts_with('/')
        || value.ends_with('/')
        || value.contains("//")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-'))
    {
        return false;
    }

    let mut segments = value.split('/');
    let Some(first) = segments.next() else {
        return false;
    };
    first != "sha256"
        && first != "."
        && first != ".."
        && segments.all(|segment| segment != "." && segment != "..")
}

pub(crate) fn is_safe_data_relative_path(value: &str) -> bool {
    if value.trim().is_empty()
        || value.len() > 200
        || value.starts_with('/')
        || value.ends_with('/')
        || value.contains("//")
        || value.contains(['\\', ':'])
        || value.chars().any(char::is_control)
    {
        return false;
    }

    value
        .split('/')
        .all(|segment| !matches!(segment, "" | "." | ".."))
        && Path::new(value)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn validate_remote_base_url(value: &str) -> Result<(), String> {
    validate_public_https_url(value, false)
        .map_err(|_| "illustrations.remote_base_url 必须是公网 HTTPS 地址".to_string())
}

/// 校验可能交给平台或 OneBot 适配器的 URL。
///
/// 此处只做语法与公网地址检查，不解析 DNS，因此不能阻止后续 DNS 重绑定；真正拉取远程
/// 媒体的组件仍须独立限制 DNS、重定向和超时。
pub(crate) fn validate_public_https_url(value: &str, allow_query: bool) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 2_048
        || value.contains('\\')
        || value.contains('(')
        || value.contains(')')
        || value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
    {
        return Err("URL 包含不允许的字符".to_string());
    }
    let path_part = value
        .split_once("://")
        .and_then(|(_, remainder)| remainder.split_once('/').map(|(_, path)| path))
        .and_then(|path| path.split(['?', '#']).next())
        .unwrap_or_default();
    if path_part
        .split('/')
        .any(|segment| matches!(segment, "." | ".."))
    {
        return Err("URL 路径包含点段".to_string());
    }
    let lowercase = value.to_ascii_lowercase();
    if ["%00", "%2f", "%2e", "%5c"]
        .iter()
        .any(|escape| lowercase.contains(escape))
    {
        return Err("URL 包含编码后的路径分隔符".to_string());
    }

    let url = Url::parse(value).map_err(|_| "URL 解析失败".to_string())?;
    if url.scheme() != "https"
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || (!allow_query && url.query().is_some())
        || url.port() == Some(0)
    {
        return Err("URL 必须是无凭据的 HTTPS 地址".to_string());
    }

    let host = url.host().ok_or_else(|| "URL 缺少主机名".to_string())?;
    if is_non_public_host(host) {
        return Err("URL 主机不是公网地址".to_string());
    }
    Ok(())
}

fn is_non_public_host(host: Host<&str>) -> bool {
    match host {
        Host::Domain(domain) => {
            let normalized = domain.trim_end_matches('.').to_ascii_lowercase();
            normalized == "localhost"
                || normalized.ends_with(".localhost")
                || normalized.ends_with(".local")
        }
        Host::Ipv4(ip) => is_non_public_ipv4(ip),
        Host::Ipv6(ip) => {
            let segments = ip.segments();
            let mapped = if segments[..5].iter().all(|segment| *segment == 0)
                && matches!(segments[5], 0 | 0xffff)
            {
                Some(std::net::Ipv4Addr::new(
                    (segments[6] >> 8) as u8,
                    segments[6] as u8,
                    (segments[7] >> 8) as u8,
                    segments[7] as u8,
                ))
            } else {
                None
            };
            mapped.is_some_and(is_non_public_ipv4)
                || ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || (segments[0] & 0xfe00 == 0xfc00)
                || (segments[0] & 0xffc0 == 0xfe80)
                || (segments[0] & 0xffc0 == 0xfec0)
                || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        }
    }
}

fn is_non_public_ipv4(ip: std::net::Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || (a == 100 && (64..=127).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || a >= 224
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_uses_safe_defaults() {
        let config = parse_config("").expect("默认配置应有效");
        assert_eq!(config, PluginConfig::default());
        assert!(!config.messages.onebot_markdown);
        assert!(config.messages.qq_official_markdown);
        assert!(config.illustrations.enabled);
        assert_eq!(config.illustrations.mode, IllustrationMode::Direct);
        assert_eq!(config.illustrations.direct_asset_root, "douluo-game/assets");
        assert!(config.identity.qq_official_account_id.is_empty());
        assert_eq!(config.authorization.mode, AuthorizationMode::AllowAll);
    }

    #[test]
    fn schema_default_is_directly_savable_and_hides_internal_storage_fields() {
        let schema: serde_json::Value = serde_json::from_str(include_str!("../config.schema.json"))
            .expect("配置 Schema 应有效");
        let ui: serde_json::Value =
            serde_json::from_str(include_str!("../config.ui.json")).expect("UI Schema 应有效");
        assert_eq!(ui["/database"]["hidden"], true);
        assert_eq!(ui["/content"]["hidden"], true);

        let default_json = serde_json::to_string(&schema["default"]).expect("根默认值应可序列化");
        let config = parse_config(&default_json).expect("首次打开表单的默认草稿应可直接保存");
        assert_eq!(config.database, DatabaseConfig::default());
        assert_eq!(config.content, ContentConfig::default());
        assert!(!config.web.enabled);
        assert_eq!(config.web.bind, "127.0.0.1");
        assert_eq!(config.web.port, 18_181);
        assert!(config.web.public_base_url.is_empty());
    }

    #[test]
    fn authorization_mode_is_explicit_and_backward_compatible() {
        assert_eq!(
            parse_config(r#"{"authorization":{"mode":"allowlist"}}"#)
                .expect("allowlist 应有效")
                .authorization
                .mode,
            AuthorizationMode::Allowlist
        );
        assert!(parse_config(r#"{"authorization":{"mode":"implicit"}}"#).is_err());
    }

    #[test]
    fn qq_official_account_fallback_is_optional_but_strict() {
        let config = parse_config(r#"{"identity":{"qq_official_account_id":"1024-app-id"}}"#)
            .expect("稳定 QQ 官方账号应有效");
        assert_eq!(config.identity.qq_official_account_id, "1024-app-id");

        for account_id in [" ", " padded", "padded ", "line\nbreak"] {
            assert!(
                parse_config(&format!(
                    r#"{{"identity":{{"qq_official_account_id":{}}}}}"#,
                    serde_json::to_string(account_id).expect("account JSON")
                ))
                .is_err(),
                "不稳定账号值不得通过：{account_id:?}"
            );
        }
        let too_long = "魂".repeat(129);
        assert!(
            parse_config(&format!(
                r#"{{"identity":{{"qq_official_account_id":{}}}}}"#,
                serde_json::to_string(&too_long).expect("account JSON")
            ))
            .is_err()
        );
        let exact_limit = "魂".repeat(128);
        assert!(
            parse_config(&format!(
                r#"{{"identity":{{"qq_official_account_id":{}}}}}"#,
                serde_json::to_string(&exact_limit).expect("account JSON")
            ))
            .is_ok()
        );
    }

    #[test]
    fn direct_asset_root_must_stay_under_data_dir() {
        for path in [
            "C:outside",
            "C:/outside",
            r"\\server\share\outside",
            r"\\?\C:\outside",
            "../outside",
            "douluo-game/./assets",
            "douluo-game//assets",
            "/absolute/assets",
        ] {
            assert!(
                parse_config(&format!(
                    r#"{{"illustrations":{{"direct_asset_root":{}}}}}"#,
                    serde_json::to_string(path).expect("path json")
                ))
                .is_err(),
                "不安全本地资源根 {path} 不应通过校验"
            );
        }

        let disabled = parse_config(r#"{"illustrations":{"direct_asset_root":""}}"#)
            .expect("空本地资源根应代表禁用预加载");
        assert!(disabled.illustrations.direct_asset_root.is_empty());
    }

    #[test]
    fn rejects_parent_database_path() {
        let error = parse_config(r#"{"database":{"relative_path":"../outside.db"}}"#)
            .expect_err("上级路径必须被拒绝");
        assert!(error.contains("安全相对路径"));

        for path in [
            "C:outside.db",
            "C:/outside.db",
            r"\\server\share\outside.db",
            r"\\?\C:\outside.db",
            "douluo-game/./douluo.db",
            "douluo-game//douluo.db",
        ] {
            assert!(
                parse_config(&format!(
                    r#"{{"database":{{"relative_path":{}}}}}"#,
                    serde_json::to_string(path).expect("path json")
                ))
                .is_err(),
                "不安全数据库路径 {path} 不应通过校验"
            );
        }
    }

    #[test]
    fn content_package_path_is_safe_and_auto_publish_requires_it() {
        let config = parse_config(
            r#"{"content":{"package_file":"douluo-game/content/example.toml","auto_publish":true}}"#,
        )
        .expect("safe content package config should parse");
        assert_eq!(
            config.content.package_file,
            "douluo-game/content/example.toml"
        );
        assert!(config.content.auto_publish);

        for path in [
            "../package.toml",
            "douluo-game/content/package.txt",
            "C:/package.json",
        ] {
            assert!(
                parse_config(&format!(
                    r#"{{"content":{{"package_file":{}}}}}"#,
                    serde_json::to_string(path).expect("content path JSON")
                ))
                .is_err(),
                "unsafe content package path {path} should be rejected"
            );
        }
        assert!(parse_config(r#"{"content":{"auto_publish":true}}"#).is_err());
    }

    #[test]
    fn web_defaults_are_disabled_and_enabled_config_is_strict() {
        let config = parse_config("{}").expect("默认配置应有效");
        assert!(!config.web.enabled);
        assert_eq!(config.web.bind, "127.0.0.1");
        assert_eq!(config.web.port, 18_181);
        assert!(format!("{config:?}").contains("<empty>"));

        let enabled = parse_config(r#"{"web":{"enabled":true,"admin_secret":"0123456789abcdef"}}"#)
            .expect("回环管理服务配置应有效");
        assert_eq!(
            enabled.web.socket_addr().expect("地址应有效").port(),
            18_181
        );
        let debug = format!("{enabled:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("0123456789abcdef"));

        for config_json in [
            r#"{"web":{"enabled":true,"admin_secret":"short"}}"#,
            r#"{"web":{"enabled":true,"bind":"example.com","admin_secret":"0123456789abcdef"}}"#,
            r#"{"web":{"enabled":true,"bind":"0.0.0.0","admin_secret":"0123456789abcdef"}}"#,
            r#"{"web":{"enabled":true,"bind":"0.0.0.0","allow_remote":true,"admin_secret":"0123456789abcdef"}}"#,
        ] {
            assert!(
                parse_config(config_json).is_err(),
                "不安全 Web 配置不应通过"
            );
        }

        let remote = parse_config(
            r#"{"web":{"enabled":true,"bind":"0.0.0.0","allow_remote":true,"public_base_url":"https://admin.example.com","admin_secret":"0123456789abcdef"}}"#,
        )
        .expect("带公开 HTTPS 基址的远程配置应有效");
        assert!(remote.web.allow_remote);
    }

    #[test]
    fn rejects_unknown_fields() {
        let error = parse_config(r#"{"unknown":true}"#).expect_err("未知字段必须被拒绝");
        assert!(error.contains("unknown field"));
    }

    #[test]
    fn remote_mode_requires_clean_https_base_url() {
        let config = parse_config(
            r#"{"illustrations":{"mode":"remote","remote_base_url":"https://media.example.com/douluo"}}"#,
        )
        .expect("公网图片服务地址应有效");
        assert_eq!(
            config
                .illustrations
                .remote_asset_url("maps/holy-soul-village.webp")
                .as_deref(),
            Some("https://media.example.com/douluo/media/maps/holy-soul-village.webp")
        );

        let error = parse_config(
            r#"{"illustrations":{"mode":"remote","remote_base_url":"http://127.0.0.1:18181"}}"#,
        )
        .expect_err("非 HTTPS 地址必须被拒绝");
        assert!(error.contains("公网 HTTPS"));

        assert!(
            parse_config(
                r#"{"illustrations":{"mode":"remote","remote_base_url":"https://127.0.0.1:18181"}}"#,
            )
            .is_err()
        );
        for host in ["127.1", "2130706433", "127.0.0.1.", "[::ffff:127.0.0.1]"] {
            assert!(
                parse_config(&format!(
                    r#"{{"illustrations":{{"mode":"remote","remote_base_url":"https://{host}"}}}}"#
                ))
                .is_err(),
                "非公网主机 {host} 不应通过校验"
            );
        }
        assert!(parse_config(
            r#"{"illustrations":{"mode":"remote","remote_base_url":"https://media.example.com/a/../douluo"}}"#
        )
        .is_err());

        for asset_key in [
            "maps/./village.png",
            "maps/../village.png",
            "maps//village.png",
            "sha256/alias.png",
            "地图/village.png",
        ] {
            assert!(
                config.illustrations.remote_asset_url(asset_key).is_none(),
                "不安全资源键 {asset_key} 不应生成 URL"
            );
        }
    }
}
