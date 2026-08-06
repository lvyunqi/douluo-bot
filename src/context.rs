use abi_stable_host_api::CommandRequest;
use serde_json::Value;

use crate::message::Protocol;

/// 规范化后的会话类型；明确区分私聊、DMS、群聊和频道，避免把未知场景当成私聊。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConversationContext {
    Private,
    Dms,
    Group { id: String },
    Channel { id: String },
}

impl ConversationContext {
    /// 返回需要进入授权表查询的目标；私聊不需要授权记录。
    pub fn authorization_target(&self) -> Option<(&'static str, &str)> {
        match self {
            Self::Group { id } => Some(("group", id.as_str())),
            Self::Channel { id } => Some(("channel", id.as_str())),
            Self::Private | Self::Dms => None,
        }
    }

    /// 返回操作日志使用的稳定会话分类。
    pub fn audit_kind(&self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Dms => "dms",
            Self::Group { .. } => "group",
            Self::Channel { .. } => "channel",
        }
    }
}

/// 从宿主规范化事件中解析当前会话；无法确认场景时安全拒绝。
pub fn resolve_conversation_context(
    request: &CommandRequest,
    protocol: Protocol,
) -> Result<ConversationContext, String> {
    match protocol {
        Protocol::OneBot11 => onebot_context(request),
        Protocol::QqOfficial => qq_context(request),
    }
}

fn onebot_context(request: &CommandRequest) -> Result<ConversationContext, String> {
    let group_id = request.group_id.as_str();
    if group_id.is_empty() {
        return Ok(ConversationContext::Private);
    }
    valid_context_id(group_id)?;
    Ok(ConversationContext::Group {
        id: group_id.to_string(),
    })
}

fn qq_context(request: &CommandRequest) -> Result<ConversationContext, String> {
    let raw: Value = serde_json::from_str(request.raw_event_json.as_str())
        .map_err(|_| "官方 QQ 原始事件不是有效 JSON，无法确认授权上下文".to_string())?;
    let payload = raw
        .get("qqbot_payload")
        .and_then(Value::as_object)
        .ok_or_else(|| "官方 QQ 原始事件缺少 qqbot_payload，无法确认授权上下文".to_string())?;
    // QimenBot 当前把事件类型放在顶层；payload 回退只用于兼容旧合成事件。
    let event_type = raw
        .get("event_type")
        .and_then(Value::as_str)
        .or_else(|| payload.get("event_type").and_then(Value::as_str))
        .ok_or_else(|| "官方 QQ 原始事件缺少 event_type，无法确认授权上下文".to_string())?;

    match event_type {
        "C2C_MESSAGE_CREATE" => Ok(ConversationContext::Private),
        "DIRECT_MESSAGE_CREATE" => Ok(ConversationContext::Dms),
        "GROUP_AT_MESSAGE_CREATE" | "GROUP_MESSAGE_CREATE" => {
            let group_id = payload
                .get("group_openid")
                .and_then(Value::as_str)
                .or_else(|| raw.get("group_openid").and_then(Value::as_str))
                .ok_or_else(|| "官方 QQ 群消息缺少 group_openid，无法确认授权上下文".to_string())?;
            valid_context_id(group_id)?;
            Ok(ConversationContext::Group {
                id: group_id.to_string(),
            })
        }
        "AT_MESSAGE_CREATE" | "MESSAGE_CREATE" => {
            let channel_id = payload
                .get("channel_id")
                .and_then(Value::as_str)
                .or_else(|| raw.get("channel_id").and_then(Value::as_str))
                .ok_or_else(|| "官方 QQ 频道消息缺少 channel_id，无法确认授权上下文".to_string())?;
            valid_context_id(channel_id)?;
            Ok(ConversationContext::Channel {
                id: channel_id.to_string(),
            })
        }
        _ => Err(format!(
            "官方 QQ 事件类型 {event_type} 不是可识别的命令会话，无法确认授权上下文"
        )),
    }
}

fn valid_context_id(value: &str) -> Result<(), String> {
    if value.is_empty() || value.chars().count() > 256 || value.chars().any(char::is_control) {
        return Err("授权上下文 ID 必须是 1 到 256 个无控制字符的字符串".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use abi_stable::std_types::RString;
    use serde_json::json;

    use super::*;

    fn request(group_id: &str, raw: Value) -> CommandRequest {
        CommandRequest {
            args: RString::new(),
            command_name: RString::from("状态"),
            sender_id: RString::from("user"),
            group_id: RString::from(group_id),
            raw_event_json: RString::from(raw.to_string()),
            sender_nickname: RString::new(),
            message_id: RString::from("message"),
            timestamp: 0,
        }
    }

    #[test]
    fn onebot_private_and_group_contexts_are_distinct() {
        assert_eq!(
            resolve_conversation_context(&request("", json!({"self_id": 1})), Protocol::OneBot11)
                .expect("私聊应解析"),
            ConversationContext::Private
        );
        assert_eq!(
            resolve_conversation_context(
                &request("group-1", json!({"self_id": 1})),
                Protocol::OneBot11
            )
            .expect("群聊应解析"),
            ConversationContext::Group {
                id: "group-1".to_string()
            }
        );
    }

    #[test]
    fn qq_group_channel_and_direct_contexts_use_event_kind() {
        assert_eq!(
            resolve_conversation_context(
                &request(
                    "group-openid",
                    json!({
                        "event_type":"GROUP_AT_MESSAGE_CREATE",
                        "group_openid":"group-openid",
                        "qqbot_payload":{"group_openid":"group-openid"}
                    })
                ),
                Protocol::QqOfficial
            )
            .expect("QQ 群应解析"),
            ConversationContext::Group {
                id: "group-openid".to_string()
            }
        );
        let channel = resolve_conversation_context(
            &request(
                "",
                json!({
                    "event_type":"AT_MESSAGE_CREATE",
                    "channel_id":"channel-1",
                    "guild_id":"guild-1",
                    "qqbot_payload":{"channel_id":"channel-1","guild_id":"guild-1"}
                }),
            ),
            Protocol::QqOfficial,
        )
        .expect("QQ 频道应解析");
        assert_eq!(
            channel,
            ConversationContext::Channel {
                id: "channel-1".to_string()
            }
        );
        assert_eq!(
            resolve_conversation_context(
                &request(
                    "",
                    json!({"event_type":"C2C_MESSAGE_CREATE","qqbot_payload":{}})
                ),
                Protocol::QqOfficial
            )
            .expect("QQ C2C 应按普通私聊处理"),
            ConversationContext::Private
        );
        assert_eq!(
            resolve_conversation_context(
                &request(
                    "",
                    json!({
                        "event_type":"DIRECT_MESSAGE_CREATE",
                        "channel_id":"dm-channel",
                        "qqbot_payload":{"channel_id":"dm-channel"}
                    })
                ),
                Protocol::QqOfficial
            )
            .expect("QQ DMS 应按私聊处理"),
            ConversationContext::Dms
        );
    }

    #[test]
    fn malformed_qq_context_fails_closed() {
        assert!(
            resolve_conversation_context(
                &request("", json!({"qqbot_payload": null})),
                Protocol::QqOfficial
            )
            .is_err()
        );
        assert!(
            resolve_conversation_context(
                &request("", json!({"qqbot_payload": {}})),
                Protocol::QqOfficial
            )
            .is_err()
        );
        assert!(
            resolve_conversation_context(
                &request(
                    "",
                    json!({"event_type":"INTERACTION_CREATE","qqbot_payload":{}})
                ),
                Protocol::QqOfficial
            )
            .is_err()
        );
        assert!(
            resolve_conversation_context(
                &request("group\n1", json!({"self_id": 1})),
                Protocol::OneBot11
            )
            .is_err()
        );
    }
}
