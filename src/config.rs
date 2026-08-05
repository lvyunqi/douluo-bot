use std::path::{Component, Path};

use serde::Deserialize;

/// 插件启动配置。所有字段都提供无配置文件时的安全默认值。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct PluginConfig {
    pub database: DatabaseConfig,
    pub identity: IdentityConfig,
    pub messages: MessageConfig,
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
    pub max_character_name_chars: usize,
}

impl Default for IdentityConfig {
    fn default() -> Self {
        Self {
            namespace: "default".to_string(),
            max_character_name_chars: 6,
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
    let path = Path::new(&config.database.relative_path);
    if config.database.relative_path.trim().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::RootDir))
    {
        return Err("database.relative_path 必须是 data_dir 内的安全相对路径".to_string());
    }
    if !(100..=30_000).contains(&config.database.busy_timeout_ms) {
        return Err("database.busy_timeout_ms 必须在 100 到 30000 之间".to_string());
    }
    if !valid_namespace(&config.identity.namespace) {
        return Err(
            "identity.namespace 只能包含字母、数字、点、下划线和横线，长度 1-64".to_string(),
        );
    }
    if !(2..=20).contains(&config.identity.max_character_name_chars) {
        return Err("identity.max_character_name_chars 必须在 2 到 20 之间".to_string());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_uses_safe_defaults() {
        let config = parse_config("").expect("默认配置应有效");
        assert_eq!(config, PluginConfig::default());
        assert!(!config.messages.onebot_markdown);
        assert!(config.messages.qq_official_markdown);
    }

    #[test]
    fn rejects_parent_database_path() {
        let error = parse_config(r#"{"database":{"relative_path":"../outside.db"}}"#)
            .expect_err("上级路径必须被拒绝");
        assert!(error.contains("安全相对路径"));
    }

    #[test]
    fn rejects_unknown_fields() {
        let error = parse_config(r#"{"unknown":true}"#).expect_err("未知字段必须被拒绝");
        assert!(error.contains("unknown field"));
    }
}
