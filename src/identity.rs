use abi_stable_host_api::CommandRequest;
use serde_json::{Map, Value};

use crate::config::IdentityConfig;
use crate::message::Protocol;

const QIMEN_CONTEXT_VERSION: u64 = 1;
const MAX_ACCOUNT_ID_CHARS: usize = 128;
const MAX_SUBJECT_ID_CHARS: usize = 256;

/// 由受信任的宿主上下文与协议原始事件共同解析出的玩家身份范围。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedIdentity {
    pub protocol: Protocol,
    pub account_id: String,
    pub subject_id: String,
}

/// 只解析当前命令所属协议，不要求有状态命令所需的稳定机器人账号。
///
/// 授权层用它先识别私聊；群聊和频道仍会在查询授权记录前调用 `resolve_identity`，
/// 因而不会用部署别名或未知账号访问持久化数据。
pub fn resolve_protocol(request: &CommandRequest) -> Result<Protocol, String> {
    let raw_event: Value = serde_json::from_str(request.raw_event_json.as_str())
        .map_err(|_| "原始事件不是有效 JSON，无法确认消息协议".to_string())?;
    let root = raw_event
        .as_object()
        .ok_or_else(|| "原始事件必须是 JSON 对象，无法确认消息协议".to_string())?;
    let context = root
        .get("qimen_context")
        .map(parse_qimen_context)
        .transpose()?;

    match context.map(|context| context.protocol) {
        Some(Protocol::OneBot11) if root.contains_key("qqbot_payload") => {
            Err("qimen_context 协议与 QQ 原始事件不一致".to_string())
        }
        Some(Protocol::QqOfficial) if root.contains_key("self_id") => {
            Err("qimen_context 协议与 OneBot 原始事件不一致".to_string())
        }
        Some(protocol) => Ok(protocol),
        None => match (
            root.contains_key("self_id"),
            root.get("qqbot_payload").is_some_and(Value::is_object),
        ) {
            (true, false) => Ok(Protocol::OneBot11),
            (false, true) => Ok(Protocol::QqOfficial),
            (true, true) => Err("原始事件同时包含 OneBot 与 QQ 官方协议标记".to_string()),
            (false, false) => Err("原始事件缺少可验证的消息协议标记".to_string()),
        },
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct QimenContext {
    protocol: Protocol,
    account_id: Option<String>,
}

/// 解析一次有状态命令的协议、机器人账号和发送者。
///
/// `bot_instance` 是部署别名，`identity.namespace` 是部署分区；二者都不是机器人账号，
/// 因此本函数不会读取或返回它们。无法证明账号归属时必须拒绝命令。
pub fn resolve_identity(
    request: &CommandRequest,
    config: &IdentityConfig,
) -> Result<ResolvedIdentity, String> {
    let subject_id = parse_subject_id(request.sender_id.as_str())?;
    let raw_event: Value = serde_json::from_str(request.raw_event_json.as_str())
        .map_err(|_| "原始事件不是有效 JSON，无法确认机器人账号".to_string())?;
    let root = raw_event
        .as_object()
        .ok_or_else(|| "原始事件必须是 JSON 对象，无法确认机器人账号".to_string())?;

    let context = root
        .get("qimen_context")
        .map(parse_qimen_context)
        .transpose()?;

    let (protocol, account_id) = match context {
        Some(context) => resolve_with_context(root, context, config)?,
        None => resolve_legacy_event(root, config)?,
    };

    Ok(ResolvedIdentity {
        protocol,
        account_id,
        subject_id,
    })
}

fn resolve_with_context(
    root: &Map<String, Value>,
    context: QimenContext,
    config: &IdentityConfig,
) -> Result<(Protocol, String), String> {
    match context.protocol {
        Protocol::OneBot11 => {
            if root.contains_key("qqbot_payload") {
                return Err("qimen_context 协议与 QQ 原始事件不一致".to_string());
            }
            let self_id = parse_onebot_self_id(root.get("self_id"))?;
            if let Some(context_account_id) = context.account_id
                && context_account_id != self_id
            {
                return Err("OneBot self_id 与 qimen_context.account_id 不一致".to_string());
            }
            Ok((Protocol::OneBot11, self_id))
        }
        Protocol::QqOfficial => {
            if root.contains_key("self_id") {
                return Err("qimen_context 协议与 OneBot 原始事件不一致".to_string());
            }
            let account_id = context.account_id.ok_or_else(|| {
                "qimen_context 缺少 QQ 官方机器人 account_id，已拒绝有状态命令".to_string()
            })?;
            let configured = optional_configured_qq_account(config)?;
            if let Some(configured) = configured
                && configured != account_id
            {
                return Err(
                    "qimen_context.account_id 与 identity.qq_official_account_id 不一致"
                        .to_string(),
                );
            }
            Ok((Protocol::QqOfficial, account_id))
        }
    }
}

fn resolve_legacy_event(
    root: &Map<String, Value>,
    config: &IdentityConfig,
) -> Result<(Protocol, String), String> {
    let has_self_id = root.contains_key("self_id");
    let has_qq_payload = root.contains_key("qqbot_payload");
    match (has_self_id, has_qq_payload) {
        (true, false) => Ok((
            Protocol::OneBot11,
            parse_onebot_self_id(root.get("self_id"))?,
        )),
        (false, true) => {
            if !root.get("qqbot_payload").is_some_and(Value::is_object) {
                return Err("qqbot_payload 必须是 JSON 对象".to_string());
            }
            let account_id = optional_configured_qq_account(config)?.ok_or_else(|| {
                "旧版 QimenBot 未提供 QQ 官方机器人 account_id；请配置 identity.qq_official_account_id"
                    .to_string()
            })?;
            Ok((Protocol::QqOfficial, account_id.to_string()))
        }
        (true, true) => Err("原始事件同时包含 OneBot 与 QQ 官方协议标记".to_string()),
        (false, false) => Err("原始事件缺少 qimen_context 和可验证的协议账号".to_string()),
    }
}

fn parse_qimen_context(value: &Value) -> Result<QimenContext, String> {
    let context = value
        .as_object()
        .ok_or_else(|| "qimen_context 必须是 JSON 对象".to_string())?;
    let version = context
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| "qimen_context.version 必须是整数 1".to_string())?;
    if version != QIMEN_CONTEXT_VERSION {
        return Err(format!(
            "不支持 qimen_context.version={version}，当前仅支持版本 {QIMEN_CONTEXT_VERSION}"
        ));
    }

    let protocol = match context.get("protocol").and_then(Value::as_str) {
        Some("onebot11") => Protocol::OneBot11,
        Some("qq-official") => Protocol::QqOfficial,
        Some(_) => return Err("qimen_context.protocol 不是插件支持的协议".to_string()),
        None => return Err("qimen_context.protocol 必须是字符串".to_string()),
    };
    let account_id = context
        .get("account_id")
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| "qimen_context.account_id 必须是字符串".to_string())
                .and_then(|value| parse_account_id(value, "qimen_context.account_id"))
        })
        .transpose()?;

    Ok(QimenContext {
        protocol,
        account_id,
    })
}

fn parse_onebot_self_id(value: Option<&Value>) -> Result<String, String> {
    let value = value.ok_or_else(|| "OneBot 原始事件缺少 self_id".to_string())?;
    match value {
        Value::String(value) => parse_account_id(value, "OneBot self_id"),
        Value::Number(value) => value
            .as_u64()
            .map(|value| value.to_string())
            .ok_or_else(|| "OneBot self_id 数字必须是无符号整数".to_string()),
        _ => Err("OneBot self_id 必须是字符串或无符号整数".to_string()),
    }
}

fn optional_configured_qq_account(config: &IdentityConfig) -> Result<Option<&str>, String> {
    if config.qq_official_account_id.is_empty() {
        Ok(None)
    } else {
        parse_account_id(
            &config.qq_official_account_id,
            "identity.qq_official_account_id",
        )?;
        Ok(Some(&config.qq_official_account_id))
    }
}

fn parse_account_id(value: &str, field: &str) -> Result<String, String> {
    if value.is_empty() {
        return Err(format!("{field} 不能为空"));
    }
    if value.chars().count() > MAX_ACCOUNT_ID_CHARS {
        return Err(format!("{field} 不能超过 {MAX_ACCOUNT_ID_CHARS} 个字符"));
    }
    if value.trim() != value || value.chars().any(char::is_control) {
        return Err(format!("{field} 不能包含首尾空白或控制字符"));
    }
    Ok(value.to_string())
}

fn parse_subject_id(value: &str) -> Result<String, String> {
    if value.is_empty() {
        return Err("sender_id 不能为空".to_string());
    }
    if value.chars().count() > MAX_SUBJECT_ID_CHARS {
        return Err(format!("sender_id 不能超过 {MAX_SUBJECT_ID_CHARS} 个字符"));
    }
    if value.trim() != value || value.chars().any(char::is_control) {
        return Err("sender_id 不能包含首尾空白或控制字符".to_string());
    }
    Ok(value.to_string())
}

#[cfg(test)]
mod tests {
    use abi_stable::std_types::RString;
    use serde_json::json;

    use super::*;

    fn request(sender_id: &str, raw_event: Value) -> CommandRequest {
        CommandRequest {
            args: RString::new(),
            command_name: RString::from("状态"),
            sender_id: RString::from(sender_id),
            group_id: RString::new(),
            raw_event_json: RString::from(raw_event.to_string()),
            sender_nickname: RString::new(),
            message_id: RString::from("message-1"),
            timestamp: 0,
        }
    }

    fn config_with_qq_account(account_id: &str) -> IdentityConfig {
        IdentityConfig {
            qq_official_account_id: account_id.to_string(),
            ..IdentityConfig::default()
        }
    }

    #[test]
    fn resolves_protocol_without_requiring_a_stable_account() {
        assert_eq!(
            resolve_protocol(&request(
                "user",
                json!({
                    "event_type": "C2C_MESSAGE_CREATE",
                    "qqbot_payload": {},
                    "qimen_context": {"version": 1, "protocol": "qq-official"}
                })
            )),
            Ok(Protocol::QqOfficial)
        );
        assert_eq!(
            resolve_protocol(&request("user", json!({"qqbot_payload": {}}))),
            Ok(Protocol::QqOfficial)
        );
        assert_eq!(
            resolve_protocol(&request(
                "user",
                json!({
                    "self_id": 10001,
                    "qimen_context": {"version": 1, "protocol": "onebot11"}
                })
            )),
            Ok(Protocol::OneBot11)
        );
    }

    #[test]
    fn resolves_onebot_context_with_numeric_or_string_self_id() {
        for (self_id, account_id, expected) in [
            (json!(2733944636_u64), "2733944636", "2733944636"),
            (json!("bot-alpha"), "bot-alpha", "bot-alpha"),
            (json!("000123"), "000123", "000123"),
        ] {
            let identity = resolve_identity(
                &request(
                    "user-1",
                    json!({
                        "self_id": self_id,
                        "qimen_context": {
                            "version": 1,
                            "protocol": "onebot11",
                            "bot_instance": "ignored-deployment-alias",
                            "account_id": account_id
                        }
                    }),
                ),
                &IdentityConfig::default(),
            )
            .expect("一致的 OneBot 上下文应解析");
            assert_eq!(identity.protocol, Protocol::OneBot11);
            assert_eq!(identity.account_id, expected);
            assert_eq!(identity.subject_id, "user-1");
        }
    }

    #[test]
    fn onebot_context_can_use_self_id_when_host_account_is_unconfigured() {
        let identity = resolve_identity(
            &request(
                "user",
                json!({
                    "self_id": "10001",
                    "qimen_context": {"version": 1, "protocol": "onebot11"}
                }),
            ),
            &IdentityConfig::default(),
        )
        .expect("OneBot self_id 本身就是可验证账号");
        assert_eq!(identity.account_id, "10001");
    }

    #[test]
    fn legacy_onebot_requires_a_strict_self_id() {
        for (self_id, expected) in [
            (json!(10001), "10001"),
            (json!(0), "0"),
            (json!("0010001"), "0010001"),
        ] {
            assert_eq!(
                resolve_identity(
                    &request("user", json!({"self_id": self_id})),
                    &IdentityConfig::default(),
                )
                .expect("旧 Host OneBot self_id 应解析")
                .account_id,
                expected
            );
        }

        for invalid in [
            Value::Null,
            json!(true),
            json!([]),
            json!({}),
            json!(1.5),
            json!(-1),
            json!(""),
            json!(" 10001"),
        ] {
            assert!(
                resolve_identity(
                    &request("user", json!({"self_id": invalid})),
                    &IdentityConfig::default(),
                )
                .is_err(),
                "不合法 self_id 不得进入共享身份桶"
            );
        }
    }

    #[test]
    fn onebot_context_requires_self_id_and_exact_account_consistency() {
        let config = IdentityConfig::default();
        for raw_event in [
            json!({
                "qimen_context": {
                    "version": 1,
                    "protocol": "onebot11",
                    "account_id": "10001"
                }
            }),
            json!({
                "self_id": 10001,
                "qimen_context": {
                    "version": 1,
                    "protocol": "onebot11",
                    "account_id": "10002"
                }
            }),
            json!({
                "self_id": 10001,
                "qqbot_payload": {},
                "qimen_context": {
                    "version": 1,
                    "protocol": "onebot11",
                    "account_id": "10001"
                }
            }),
        ] {
            assert!(resolve_identity(&request("user", raw_event), &config).is_err());
        }
    }

    #[test]
    fn qq_context_account_is_authoritative_and_config_must_agree() {
        let raw_event = json!({
            "qqbot_payload": {"id": "message"},
            "qimen_context": {
                "version": 1,
                "protocol": "qq-official",
                "bot_instance": "qq-main",
                "account_id": "1024-app-id"
            }
        });
        let identity = resolve_identity(
            &request("member-openid", raw_event.clone()),
            &IdentityConfig::default(),
        )
        .expect("新 Host 的账号上下文应独立工作");
        assert_eq!(identity.protocol, Protocol::QqOfficial);
        assert_eq!(identity.account_id, "1024-app-id");

        resolve_identity(
            &request("member-openid", raw_event.clone()),
            &config_with_qq_account("1024-app-id"),
        )
        .expect("兼容配置与 Host 账号一致时应通过");
        assert!(
            resolve_identity(
                &request("member-openid", raw_event),
                &config_with_qq_account("different-app-id"),
            )
            .is_err()
        );
    }

    #[test]
    fn qq_context_never_falls_back_when_account_is_missing() {
        let raw_event = json!({
            "qqbot_payload": {},
            "qimen_context": {"version": 1, "protocol": "qq-official"}
        });
        assert!(
            resolve_identity(
                &request("member-openid", raw_event),
                &config_with_qq_account("legacy-single-bot"),
            )
            .is_err()
        );
    }

    #[test]
    fn legacy_qq_requires_explicit_single_bot_fallback() {
        let raw_event = json!({"qqbot_payload": {"id": "message"}});
        let identity = resolve_identity(
            &request("member-openid", raw_event.clone()),
            &config_with_qq_account("legacy-app-id"),
        )
        .expect("明确配置的旧 Host 单 Bot fallback 应工作");
        assert_eq!(identity.protocol, Protocol::QqOfficial);
        assert_eq!(identity.account_id, "legacy-app-id");
        assert!(
            resolve_identity(
                &request("member-openid", raw_event),
                &IdentityConfig::default(),
            )
            .is_err()
        );
        assert!(
            resolve_identity(
                &request("member-openid", json!({"qqbot_payload": null})),
                &config_with_qq_account("legacy-app-id"),
            )
            .is_err()
        );
    }

    #[test]
    fn malformed_or_unsupported_qimen_context_fails_closed() {
        let too_long_account = "魂".repeat(129);
        for context in [
            Value::Null,
            json!([]),
            json!({}),
            json!({"version": "1", "protocol": "onebot11"}),
            json!({"version": 2, "protocol": "onebot11"}),
            json!({"version": 1}),
            json!({"version": 1, "protocol": "onebot12"}),
            json!({"version": 1, "protocol": "qq-official", "account_id": 123}),
            json!({"version": 1, "protocol": "qq-official", "account_id": " padded "}),
            json!({"version": 1, "protocol": "qq-official", "account_id": too_long_account}),
        ] {
            assert!(
                resolve_identity(
                    &request(
                        "user",
                        json!({
                            "self_id": 10001,
                            "qimen_context": context
                        }),
                    ),
                    &config_with_qq_account("legacy-app-id"),
                )
                .is_err(),
                "畸形或未知版本上下文不得降级到旧解析"
            );
        }
    }

    #[test]
    fn protocol_ambiguity_and_missing_evidence_fail_closed() {
        let config = config_with_qq_account("qq-app");
        for raw_event in [
            json!({}),
            json!({"self_id": 10001, "qqbot_payload": {}}),
            json!({
                "self_id": 10001,
                "qimen_context": {
                    "version": 1,
                    "protocol": "qq-official",
                    "account_id": "qq-app"
                }
            }),
        ] {
            assert!(resolve_identity(&request("user", raw_event), &config).is_err());
        }
    }

    #[test]
    fn invalid_raw_json_or_sender_id_fails_closed() {
        let mut invalid_json = request("user", json!({}));
        invalid_json.raw_event_json = RString::from("not-json");
        assert!(resolve_identity(&invalid_json, &IdentityConfig::default()).is_err());

        let array_root = request("user", json!([]));
        assert!(resolve_identity(&array_root, &IdentityConfig::default()).is_err());

        for sender_id in ["", "   ", " padded", "line\nbreak"] {
            assert!(
                resolve_identity(
                    &request(sender_id, json!({"self_id": 10001})),
                    &IdentityConfig::default(),
                )
                .is_err()
            );
        }

        let maximum_sender = "魂".repeat(256);
        assert!(
            resolve_identity(
                &request(&maximum_sender, json!({"self_id": 10001})),
                &IdentityConfig::default(),
            )
            .is_ok()
        );
        let oversized_sender = "魂".repeat(257);
        assert!(
            resolve_identity(
                &request(&oversized_sender, json!({"self_id": 10001})),
                &IdentityConfig::default(),
            )
            .is_err()
        );
    }
}
