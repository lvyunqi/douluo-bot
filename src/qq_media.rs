use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};

use abi_stable_host_api::{BotApi, CommandRequest, InterceptorRequest};
use serde_json::{Map, Value};

use crate::config::IllustrationConfig;
use crate::message::{self, GameDocument, Protocol};

/// 尚未经过宿主主回复确认的官方 QQ 本地图片最多保留八条，避免异常链路无界占用内存。
const MAX_PENDING_QQ_INLINE_IMAGES: usize = 8;

static PENDING_QQ_INLINE_IMAGES: OnceLock<Mutex<VecDeque<PendingQqInlineImage>>> = OnceLock::new();

#[derive(Clone, PartialEq, Eq)]
enum QqMediaTarget {
    Group(String),
    Private(String),
}

#[derive(Clone, PartialEq, Eq)]
struct PendingQqInlineImageKey {
    account_id: String,
    message_id: String,
    sender_id: String,
    target: QqMediaTarget,
}

pub(crate) struct PendingQqInlineImage {
    key: PendingQqInlineImageKey,
    bytes: Arc<[u8]>,
}

fn pending_images() -> &'static Mutex<VecDeque<PendingQqInlineImage>> {
    PENDING_QQ_INLINE_IMAGES.get_or_init(|| Mutex::new(VecDeque::new()))
}

/// 初始化、热重载和关闭时清空临时图片，避免旧运行时向新运行时泄漏媒体。
pub(crate) fn clear_pending_images() {
    if let Ok(mut pending) = pending_images().lock() {
        pending.clear();
    }
}

/// 仅为官方群/C2C、direct 本地插图登记主回复成功后的独立媒体。
pub(crate) fn stage_command_image(
    request: &CommandRequest,
    document: &GameDocument,
    illustrations: &IllustrationConfig,
) {
    let Some(key) = key_from_parts(
        request.raw_event_json.as_str(),
        request.sender_id.as_str(),
        request.message_id.as_str(),
    ) else {
        return;
    };
    let Some(bytes) = message::qq_official_direct_inline_image(document, illustrations) else {
        return;
    };
    stage_image(key, bytes);
}

/// 取出与当前完成回调严格匹配的图片；未命中时保持无副作用。
pub(crate) fn take_after_completion(request: &InterceptorRequest) -> Option<PendingQqInlineImage> {
    take_image(key_from_parts(
        request.raw_event_json.as_str(),
        request.sender_id.as_str(),
        request.message_id.as_str(),
    )?)
}

/// 丢弃快捷键无法安全回复时留下的临时图片，避免后续无正文发送。
pub(crate) fn discard_for_interceptor(request: &InterceptorRequest) {
    let Some(key) = key_from_parts(
        request.raw_event_json.as_str(),
        request.sender_id.as_str(),
        request.message_id.as_str(),
    ) else {
        return;
    };
    let Ok(mut pending) = pending_images().lock() else {
        return;
    };
    pending.retain(|entry| entry.key != key);
}

/// 将已确认的图片写入当前动态回调的宿主发送队列。
pub(crate) fn send_image(image: PendingQqInlineImage) {
    let segments_json = message::qq_official_inline_image_segments(&image.bytes);
    match image.key.target {
        QqMediaTarget::Group(group_id) => BotApi::send_group_rich(&group_id, &segments_json),
        QqMediaTarget::Private(user_id) => BotApi::send_private_rich(&user_id, &segments_json),
    }
}

fn stage_image(key: PendingQqInlineImageKey, bytes: Arc<[u8]>) {
    let Ok(mut pending) = pending_images().lock() else {
        return;
    };
    pending.retain(|entry| entry.key != key);
    while pending.len() >= MAX_PENDING_QQ_INLINE_IMAGES {
        pending.pop_front();
    }
    pending.push_back(PendingQqInlineImage { key, bytes });
}

fn take_image(key: PendingQqInlineImageKey) -> Option<PendingQqInlineImage> {
    let Ok(mut pending) = pending_images().lock() else {
        return None;
    };
    let position = pending.iter().position(|entry| entry.key == key)?;
    pending.remove(position)
}

/// 从宿主可信上下文建立匹配键；不使用可变的 `bot_id` 作为账号身份。
fn key_from_parts(
    raw_event_json: &str,
    sender_id: &str,
    message_id: &str,
) -> Option<PendingQqInlineImageKey> {
    if message::detect_protocol(raw_event_json) != Protocol::QqOfficial {
        return None;
    }
    let raw_event = serde_json::from_str::<Value>(raw_event_json).ok()?;
    let root = raw_event.as_object()?;
    let payload = root.get("qqbot_payload")?.as_object()?;
    let context = root.get("qimen_context")?.as_object()?;
    if context.get("protocol").and_then(Value::as_str) != Some("qq-official") {
        return None;
    }
    let account_id = valid_id(context.get("account_id")?.as_str()?)?;
    let sender_id = valid_id(sender_id)?;
    let message_id = valid_id(message_id)?;
    let event_type = string_field(root, payload, "event_type")?;
    let target = match event_type {
        "GROUP_AT_MESSAGE_CREATE" | "GROUP_MESSAGE_CREATE" => QqMediaTarget::Group(
            valid_id(string_field(root, payload, "group_openid")?)?.to_string(),
        ),
        "C2C_MESSAGE_CREATE" => QqMediaTarget::Private(sender_id.to_string()),
        "AT_MESSAGE_CREATE" | "MESSAGE_CREATE" | "DIRECT_MESSAGE_CREATE" => return None,
        _ => return None,
    };
    Some(PendingQqInlineImageKey {
        account_id: account_id.to_string(),
        message_id: message_id.to_string(),
        sender_id: sender_id.to_string(),
        target,
    })
}

fn string_field<'a>(
    root: &'a Map<String, Value>,
    payload: &'a Map<String, Value>,
    name: &str,
) -> Option<&'a str> {
    root.get(name)
        .and_then(Value::as_str)
        .or_else(|| payload.get(name).and_then(Value::as_str))
}

fn valid_id(value: &str) -> Option<&str> {
    (!value.is_empty() && value.trim() == value && !value.chars().any(char::is_control))
        .then_some(value)
}

#[cfg(test)]
fn pending_image_count() -> usize {
    pending_images()
        .lock()
        .map(|pending| pending.len())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};

    use abi_stable::std_types::RString;
    use abi_stable_host_api::{CommandResponse, drain_send_queue};
    use serde_json::json;

    use super::*;
    use crate::message::Illustration;

    static QUEUE_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn queue_test_lock() -> std::sync::MutexGuard<'static, ()> {
        QUEUE_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("QQ 图片队列测试锁应可用")
    }

    fn direct_document() -> GameDocument {
        GameDocument::new("角色状态")
            .line("完整文字不能丢失")
            .illustration(
                Illustration::inline_image_bytes("状态卡片", b"GIF89a", 1, 1)
                    .expect("测试 GIF 应可内联"),
            )
    }

    fn raw_event(event_type: &str) -> String {
        let payload = match event_type {
            "GROUP_AT_MESSAGE_CREATE" => json!({"group_openid":"group-openid"}),
            "C2C_MESSAGE_CREATE" => json!({"openid":"user-openid"}),
            "AT_MESSAGE_CREATE" => json!({"channel_id":"channel-id","guild_id":"guild-id"}),
            "DIRECT_MESSAGE_CREATE" => {
                json!({"channel_id":"dms-id","guild_id":"guild-id"})
            }
            _ => json!({}),
        };
        json!({
            "event_type": event_type,
            "qqbot_payload": payload,
            "qimen_context": {
                "version": 1,
                "protocol": "qq-official",
                "account_id": "qq-account"
            }
        })
        .to_string()
    }

    fn command_request(event_type: &str, message_id: &str) -> CommandRequest {
        CommandRequest {
            args: RString::new(),
            command_name: RString::from("状态"),
            sender_id: RString::from("user-openid"),
            group_id: RString::from("group-openid"),
            raw_event_json: RString::from(raw_event(event_type)),
            sender_nickname: RString::from("测试者"),
            message_id: RString::from(message_id),
            timestamp: 0,
        }
    }

    fn interceptor_request(event_type: &str, message_id: &str) -> InterceptorRequest {
        InterceptorRequest {
            bot_id: RString::from("可变部署别名"),
            sender_id: RString::from("user-openid"),
            group_id: RString::from("group-openid"),
            message_text: RString::from("状态"),
            raw_event_json: RString::from(raw_event(event_type)),
            sender_nickname: RString::from("测试者"),
            message_id: RString::from(message_id),
            timestamp: 0,
        }
    }

    #[test]
    fn only_group_and_c2c_stage_direct_inline_images() {
        let _guard = queue_test_lock();
        clear_pending_images();
        let document = direct_document();
        for event_type in [
            "GROUP_AT_MESSAGE_CREATE",
            "C2C_MESSAGE_CREATE",
            "AT_MESSAGE_CREATE",
            "DIRECT_MESSAGE_CREATE",
        ] {
            let request = command_request(event_type, event_type);
            stage_command_image(&request, &document, &IllustrationConfig::default());
        }
        assert_eq!(pending_image_count(), 2);
        assert!(
            take_after_completion(&interceptor_request(
                "GROUP_AT_MESSAGE_CREATE",
                "GROUP_AT_MESSAGE_CREATE"
            ))
            .is_some()
        );
        assert!(
            take_after_completion(&interceptor_request(
                "C2C_MESSAGE_CREATE",
                "C2C_MESSAGE_CREATE"
            ))
            .is_some()
        );
        assert!(
            take_after_completion(&interceptor_request(
                "AT_MESSAGE_CREATE",
                "AT_MESSAGE_CREATE"
            ))
            .is_none()
        );
        assert!(
            take_after_completion(&interceptor_request(
                "DIRECT_MESSAGE_CREATE",
                "DIRECT_MESSAGE_CREATE"
            ))
            .is_none()
        );
        clear_pending_images();
    }

    #[test]
    fn normal_command_image_is_sent_only_after_completion() {
        let _guard = queue_test_lock();
        clear_pending_images();
        let _ = drain_send_queue();
        let command = command_request("GROUP_AT_MESSAGE_CREATE", "normal-message");
        stage_command_image(&command, &direct_document(), &IllustrationConfig::default());
        assert!(drain_send_queue().is_empty());

        let completion = interceptor_request("GROUP_AT_MESSAGE_CREATE", "normal-message");
        send_image(take_after_completion(&completion).expect("完成回调应取到对应待发送图片"));
        let sends = drain_send_queue();
        assert_eq!(sends.len(), 1);
        assert_eq!(sends[0].message_type.as_str(), "group");
        assert_eq!(sends[0].target_id.as_str(), "group-openid");
        assert!(sends[0].segments_json.contains("base64://"));
        clear_pending_images();
    }

    #[test]
    fn player_alias_queues_text_before_its_pending_image() {
        let _guard = queue_test_lock();
        clear_pending_images();
        let _ = drain_send_queue();
        let command = command_request("GROUP_AT_MESSAGE_CREATE", "alias-message");
        let interceptor = interceptor_request("GROUP_AT_MESSAGE_CREATE", "alias-message");
        stage_command_image(&command, &direct_document(), &IllustrationConfig::default());
        assert!(crate::queue_interceptor_response(
            &interceptor,
            &CommandResponse::text("快捷键文字回执")
        ));
        send_image(take_after_completion(&interceptor).expect("快捷键应取到对应待发送图片"));

        let sends = drain_send_queue();
        assert_eq!(sends.len(), 2);
        assert!(sends[0].segments_json.contains("快捷键文字回执"));
        assert!(!sends[0].segments_json.contains("base64://"));
        assert!(sends[1].segments_json.contains("base64://"));
        clear_pending_images();
    }

    #[test]
    fn pending_images_are_bounded_and_evict_the_oldest() {
        let _guard = queue_test_lock();
        clear_pending_images();
        let document = direct_document();
        for index in 0..=MAX_PENDING_QQ_INLINE_IMAGES {
            stage_command_image(
                &command_request("GROUP_AT_MESSAGE_CREATE", &format!("message-{index}")),
                &document,
                &IllustrationConfig::default(),
            );
        }
        assert_eq!(pending_image_count(), MAX_PENDING_QQ_INLINE_IMAGES);
        assert!(
            take_after_completion(&interceptor_request("GROUP_AT_MESSAGE_CREATE", "message-0"))
                .is_none()
        );
        assert!(
            take_after_completion(&interceptor_request(
                "GROUP_AT_MESSAGE_CREATE",
                &format!("message-{MAX_PENDING_QQ_INLINE_IMAGES}")
            ))
            .is_some()
        );
        clear_pending_images();
    }
}
