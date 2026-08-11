use std::{fmt, sync::Arc};

use abi_stable_host_api::{CommandRequest, CommandResponse, DynamicActionResponse};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

use crate::config::{
    IllustrationConfig, IllustrationMode, MessageConfig, is_safe_asset_key,
    validate_public_https_url,
};

/// 领域用例输出的协议无关文档。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GameDocument {
    title: String,
    lines: Vec<String>,
    fields: Vec<(String, String)>,
    commands: Vec<String>,
    notice: Option<String>,
    illustration: Option<Illustration>,
}

impl GameDocument {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            ..Self::default()
        }
    }

    pub fn line(mut self, line: impl Into<String>) -> Self {
        self.lines.push(line.into());
        self
    }

    pub fn field(mut self, label: impl Into<String>, value: impl Into<String>) -> Self {
        self.fields.push((label.into(), value.into()));
        self
    }

    pub fn command(mut self, command: impl Into<String>) -> Self {
        self.commands.push(command.into());
        self
    }

    /// Adds a command together with a short, user-facing explanation.
    ///
    /// Keeping the pair in the command list means both the plain-text and
    /// Markdown renderers apply the same escaping and list formatting.
    pub fn command_help(
        mut self,
        command: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        self.commands
            .push(format!("{}：{}", command.into(), description.into()));
        self
    }

    pub fn notice(mut self, notice: impl Into<String>) -> Self {
        self.notice = Some(notice.into());
        self
    }

    /// 设置消息主插图；一条聊天回复只携带一张，图集通过分页命令展示。
    pub fn illustration(mut self, illustration: Illustration) -> Self {
        self.illustration = Some(illustration);
        self
    }

    pub fn illustration_if(self, illustration: Option<Illustration>) -> Self {
        match illustration {
            Some(illustration) => self.illustration(illustration),
            None => self,
        }
    }

    #[cfg(test)]
    pub(crate) fn has_illustration(&self) -> bool {
        self.illustration.is_some()
    }
}

/// 插图元数据与受校验来源。原始字节只在插件内存中存在，跨 FFI 的是编码后消息段。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Illustration {
    alt: String,
    source: IllustrationSource,
    width: u16,
    height: u16,
}

#[derive(Clone, PartialEq, Eq)]
enum IllustrationSource {
    DirectHttps(String),
    RemoteAsset(String),
    InlineImage(Arc<[u8]>),
}

impl fmt::Debug for IllustrationSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DirectHttps(_) => formatter.write_str("DirectHttps(<validated URL>)"),
            Self::RemoteAsset(asset_key) => formatter
                .debug_tuple("RemoteAsset")
                .field(asset_key)
                .finish(),
            Self::InlineImage(bytes) => formatter
                .debug_struct("InlineImage")
                .field("byte_length", &bytes.len())
                .finish(),
        }
    }
}

/// 插件主动内联图片的运营上限，低于 QimenBot/QQ 当前 20,000,000 字节协议上限。
pub const MAX_INLINE_IMAGE_BYTES: usize = 8 * 1024 * 1024;

enum ResolvedIllustration<'a> {
    Url(String),
    InlineImage(&'a [u8]),
}

impl Illustration {
    pub fn https(
        alt: impl Into<String>,
        https_url: impl Into<String>,
        width: u16,
        height: u16,
    ) -> Result<Self, String> {
        let alt = alt.into();
        let https_url = https_url.into();
        validate_https_image_url(&https_url)?;
        validate_illustration_metadata(&alt, width, height)?;
        Ok(Self {
            alt,
            source: IllustrationSource::DirectHttps(https_url),
            width,
            height,
        })
    }

    pub fn remote_asset(
        alt: impl Into<String>,
        asset_key: impl Into<String>,
        width: u16,
        height: u16,
    ) -> Result<Self, String> {
        let alt = alt.into();
        let asset_key = asset_key.into();
        validate_illustration_metadata(&alt, width, height)?;
        validate_asset_key(&asset_key)?;
        Ok(Self {
            alt,
            source: IllustrationSource::RemoteAsset(asset_key),
            width,
            height,
        })
    }

    /// 使用可信的本地或生成图片字节。只接受常见位图签名，不接受路径或 SVG。
    pub fn inline_image_bytes(
        alt: impl Into<String>,
        bytes: impl AsRef<[u8]>,
        width: u16,
        height: u16,
    ) -> Result<Self, String> {
        Self::inline_image_arc(alt, Arc::from(bytes.as_ref()), width, height)
    }

    pub(crate) fn inline_image_arc(
        alt: impl Into<String>,
        bytes: Arc<[u8]>,
        width: u16,
        height: u16,
    ) -> Result<Self, String> {
        let alt = alt.into();
        validate_illustration_metadata(&alt, width, height)?;
        validate_inline_image_bytes(&bytes)?;
        Ok(Self {
            alt,
            source: IllustrationSource::InlineImage(bytes),
            width,
            height,
        })
    }

    fn resolve<'a>(&'a self, config: &IllustrationConfig) -> Option<ResolvedIllustration<'a>> {
        if !config.enabled {
            return None;
        }
        match (&self.source, config.mode) {
            (IllustrationSource::DirectHttps(url), IllustrationMode::Direct) => {
                Some(ResolvedIllustration::Url(url.clone()))
            }
            (IllustrationSource::RemoteAsset(key), IllustrationMode::Remote) => {
                config.remote_asset_url(key).map(ResolvedIllustration::Url)
            }
            (IllustrationSource::InlineImage(bytes), IllustrationMode::Direct) => {
                Some(ResolvedIllustration::InlineImage(bytes))
            }
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    OneBot11,
    QqOfficial,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OneBot11 => "onebot11",
            Self::QqOfficial => "qq-official",
        }
    }
}

/// QQ 官方适配的规范化原始事件包含 qqbot_payload；解析失败时安全降级为 OneBot 文本。
pub fn detect_protocol(raw_event_json: &str) -> Protocol {
    serde_json::from_str::<Value>(raw_event_json)
        .ok()
        .and_then(|value| value.get("qqbot_payload").cloned())
        .map_or(Protocol::OneBot11, |_| Protocol::QqOfficial)
}

pub fn response_for(
    req: &CommandRequest,
    document: &GameDocument,
    messages: &MessageConfig,
    illustrations: &IllustrationConfig,
) -> CommandResponse {
    let protocol = detect_protocol(req.raw_event_json.as_str());
    let markdown = match protocol {
        Protocol::OneBot11 => messages.onebot_markdown,
        Protocol::QqOfficial => messages.qq_official_markdown,
    };
    let resolved_illustration = document
        .illustration
        .as_ref()
        .and_then(|illustration| illustration.resolve(illustrations));
    match protocol {
        Protocol::QqOfficial => {
            // 群/C2C 的 Markdown(msg_type=2) 与本地媒体(msg_type=7)互斥；频道/DMS
            // 尚未真实验收。内联图片首版统一保留完整文字，不冒险丢失正文。
            let illustration_url = match resolved_illustration.as_ref() {
                Some(ResolvedIllustration::Url(url)) => Some(url.as_str()),
                _ => None,
            };
            if markdown {
                markdown_response(document, illustration_url)
            } else {
                CommandResponse::text(&render_text(document))
            }
        }
        Protocol::OneBot11 => match resolved_illustration.as_ref() {
            Some(illustration) => onebot_illustrated_response(document, markdown, illustration),
            None if markdown => markdown_response(document, None),
            None => CommandResponse::text(&render_text(document)),
        },
    }
}

/// 仅为 QQ 官方群/C2C 的第二条独立媒体消息提取 direct 本地插图。
///
/// 正常主回复仍由 `response_for` 生成完整 Markdown 或文字；调用方只能在宿主确认主回复
/// 成功后，才将返回的字节交给官方 QQ 媒体发送队列。
pub(crate) fn qq_official_direct_inline_image(
    document: &GameDocument,
    illustrations: &IllustrationConfig,
) -> Option<Arc<[u8]>> {
    if !illustrations.enabled || illustrations.mode != IllustrationMode::Direct {
        return None;
    }
    match document.illustration.as_ref()?.source {
        IllustrationSource::InlineImage(ref bytes) => Some(Arc::clone(bytes)),
        IllustrationSource::DirectHttps(_) | IllustrationSource::RemoteAsset(_) => None,
    }
}

/// 构造单独的 QQ 官方本地图片段，不把 Base64 写入日志、持久化状态或普通主回复。
pub(crate) fn qq_official_inline_image_segments(bytes: &[u8]) -> String {
    json!([{
        "type": "image",
        "data": { "file": format!("base64://{}", STANDARD.encode(bytes)) }
    }])
    .to_string()
}

pub fn render_text(document: &GameDocument) -> String {
    let mut output = vec![format!("== {} ==", document.title)];
    output.extend(document.lines.iter().cloned());
    output.extend(
        document
            .fields
            .iter()
            .map(|(label, value)| format!("{label}：{value}")),
    );
    if !document.commands.is_empty() {
        output.push("可用命令：".to_string());
        output.extend(
            document
                .commands
                .iter()
                .map(|command| format!("- {command}")),
        );
    }
    if let Some(notice) = &document.notice {
        output.push(format!("提示：{notice}"));
    }
    output.join("\n")
}

pub fn render_markdown(document: &GameDocument, illustration_url: Option<&str>) -> String {
    let mut output = vec![format!("# {}", escape_markdown(&document.title))];
    if let (Some(illustration), Some(url)) = (&document.illustration, illustration_url) {
        output.push(markdown_image(illustration, url));
    }
    output.extend(document.lines.iter().map(|line| escape_markdown(line)));
    output.extend(document.fields.iter().map(|(label, value)| {
        format!(
            "- **{}**：{}",
            escape_markdown(label),
            escape_markdown(value)
        )
    }));
    if !document.commands.is_empty() {
        output.push("---".to_string());
        output.push("## 可用命令".to_string());
        output.extend(
            document
                .commands
                .iter()
                .map(|command| format!("- {}", escape_markdown(command))),
        );
    }
    if let Some(notice) = &document.notice {
        output.push(format!("> {}", escape_markdown(notice)));
    }
    output.join("\n")
}

fn markdown_response(document: &GameDocument, illustration_url: Option<&str>) -> CommandResponse {
    let segments = json!([{
        "type": "markdown",
        "data": { "content": render_markdown(document, illustration_url) }
    }]);
    CommandResponse {
        action: DynamicActionResponse::rich_reply(&segments.to_string()),
    }
}

fn onebot_illustrated_response(
    document: &GameDocument,
    markdown: bool,
    illustration: &ResolvedIllustration<'_>,
) -> CommandResponse {
    let source = match illustration {
        ResolvedIllustration::Url(url) => url.clone(),
        ResolvedIllustration::InlineImage(bytes) => {
            format!("base64://{}", STANDARD.encode(bytes))
        }
    };
    let body = if markdown {
        json!({"type": "markdown", "data": {"content": render_markdown(document, None)}})
    } else {
        json!({"type": "text", "data": {"text": render_text(document)}})
    };
    let segments = json!([
        body,
        {"type": "image", "data": {"file": source}}
    ]);
    CommandResponse {
        action: DynamicActionResponse::rich_reply(&segments.to_string()),
    }
}

fn markdown_image(illustration: &Illustration, illustration_url: &str) -> String {
    format!(
        "![{} #{}px #{}px]({})",
        escape_markdown(&illustration.alt),
        illustration.width,
        illustration.height,
        illustration_url
    )
}

fn validate_https_image_url(value: &str) -> Result<(), String> {
    validate_public_https_url(value, true)
        .map_err(|_| "QQ 官方 Markdown 插图必须使用公网 HTTPS 地址".to_string())
}

fn validate_illustration_metadata(alt: &str, width: u16, height: u16) -> Result<(), String> {
    if alt.trim().is_empty()
        || alt.chars().count() > 80
        || alt.chars().any(|character| character.is_control())
    {
        return Err("插图说明长度必须在 1 到 80 个字符之间".to_string());
    }
    if width == 0 || height == 0 || width > 4096 || height > 4096 {
        return Err("插图宽高必须在 1 到 4096 像素之间".to_string());
    }
    Ok(())
}

fn validate_asset_key(value: &str) -> Result<(), String> {
    if !is_safe_asset_key(value) {
        return Err("远程插图 asset_key 格式无效".to_string());
    }
    Ok(())
}

fn validate_inline_image_bytes(bytes: &[u8]) -> Result<(), String> {
    if bytes.is_empty() || bytes.len() > MAX_INLINE_IMAGE_BYTES {
        return Err(format!(
            "内联图片大小必须在 1 字节到 {} MiB 之间",
            MAX_INLINE_IMAGE_BYTES / 1024 / 1024
        ));
    }
    let recognized = (bytes.len() >= 8 && bytes[..8] == [137, 80, 78, 71, 13, 10, 26, 10])
        || (bytes.len() >= 3 && bytes[..3] == [0xff, 0xd8, 0xff])
        || (bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP")
        || (bytes.len() >= 6 && (&bytes[..6] == b"GIF87a" || &bytes[..6] == b"GIF89a"))
        || (bytes.len() >= 2 && &bytes[..2] == b"BM");
    if !recognized {
        return Err("内联图片必须是 PNG、JPEG、WebP、GIF 或 BMP 位图".to_string());
    }
    Ok(())
}

fn escape_markdown(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(
            character,
            '\\' | '`'
                | '*'
                | '_'
                | '{'
                | '}'
                | '['
                | ']'
                | '<'
                | '>'
                | '#'
                | '+'
                | '-'
                | '!'
                | '|'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_official_event_without_numeric_id_assumptions() {
        assert_eq!(
            detect_protocol(r#"{"qqbot_payload":{"author":{"member_openid":"abc"}}}"#),
            Protocol::QqOfficial
        );
        assert_eq!(
            detect_protocol(r#"{"post_type":"message","self_id":123}"#),
            Protocol::OneBot11
        );
    }

    #[test]
    fn markdown_escapes_player_control_characters() {
        let document = GameDocument::new("状态")
            .field("角色", "[伪造链接](https://example.com)")
            .command("状态");
        let output = render_markdown(&document, None);
        assert!(output.contains(r"\[伪造链接\]"));
        assert!(!output.contains("[伪造链接]("));
    }

    #[test]
    fn text_renderer_keeps_legacy_command_shape() {
        let output = render_text(
            &GameDocument::new("斗罗系统")
                .line("欢迎来到斗罗大陆")
                .command("开始穿越 <角色名> <男|女>"),
        );
        assert!(output.starts_with("== 斗罗系统 =="));
        assert!(output.contains("- 开始穿越"));
    }

    #[test]
    fn renders_official_markdown_image_with_dimensions() {
        let illustration = Illustration::https(
            "圣魂村地图",
            "https://media.example.com/maps/holy-soul-village.webp",
            640,
            360,
        )
        .expect("公网图片应有效");
        let output = render_markdown(
            &GameDocument::new("当前位置").illustration(illustration),
            Some("https://media.example.com/maps/holy-soul-village.webp"),
        );
        assert!(output.contains(
            "![圣魂村地图 #640px #360px](https://media.example.com/maps/holy-soul-village.webp)"
        ));
    }

    #[test]
    fn rejects_unsafe_markdown_image_sources() {
        assert!(Illustration::https("地图", "http://example.com/map.png", 640, 360).is_err());
        assert!(Illustration::https("地图", "https://user@example.com/map.png", 640, 360).is_err());
        assert!(Illustration::https("地图", "https://127.0.0.1/map.png", 640, 360).is_err());
        for host in ["127.1", "2130706433", "127.0.0.1.", "[::ffff:127.0.0.1]"] {
            assert!(
                Illustration::https("地图", format!("https://{host}/map.png"), 640, 360).is_err(),
                "非公网主机 {host} 不应通过校验"
            );
        }
        assert!(Illustration::https("地图", "https://example.com/a(b).png", 640, 360).is_err());
        assert!(Illustration::https("地图", "https://example.com/map.png", 0, 360).is_err());
        assert!(
            Illustration::https("地图\n伪造", "https://example.com/map.png", 640, 360).is_err()
        );
    }

    #[test]
    fn onebot_uses_text_and_image_segments() {
        use abi_stable::std_types::RString;

        let request = CommandRequest {
            args: RString::new(),
            command_name: RString::from("位置"),
            sender_id: RString::from("123"),
            group_id: RString::from("456"),
            raw_event_json: RString::from(r#"{"post_type":"message"}"#),
            sender_nickname: RString::from("测试者"),
            message_id: RString::from("789"),
            timestamp: 0,
        };
        let document = GameDocument::new("当前位置").illustration(
            Illustration::https("圣魂村", "https://media.example.com/map.webp", 640, 360)
                .expect("公网图片应有效"),
        );
        let response = response_for(
            &request,
            &document,
            &MessageConfig::default(),
            &IllustrationConfig::default(),
        );
        let segments: Value =
            serde_json::from_str(response.action.segments_json.as_str()).expect("消息段应为 JSON");
        assert_eq!(segments[0]["type"], "text");
        assert_eq!(segments[1]["type"], "image");
        assert_eq!(
            segments[1]["data"]["file"],
            "https://media.example.com/map.webp"
        );
    }

    #[test]
    fn remote_mode_resolves_asset_key_through_configured_service() {
        let illustration =
            Illustration::remote_asset("独狼武魂", "wuhun/lone-wolf/portrait.webp", 640, 640)
                .expect("资源键应有效");
        let config = IllustrationConfig {
            enabled: true,
            mode: IllustrationMode::Remote,
            remote_base_url: "https://media.example.com/douluo".to_string(),
            ..IllustrationConfig::default()
        };
        match illustration.resolve(&config) {
            Some(ResolvedIllustration::Url(url)) => assert_eq!(
                url,
                "https://media.example.com/douluo/media/wuhun/lone-wolf/portrait.webp"
            ),
            _ => panic!("远程资源键应解析为 URL"),
        }
        assert!(Illustration::remote_asset("非法", "../secret.png", 10, 10).is_err());
    }

    #[test]
    fn onebot_sends_inline_image_as_base64_segment() {
        use abi_stable::std_types::RString;

        let image = STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
            .expect("测试 PNG 应可解码");
        let illustration =
            Illustration::inline_image_bytes("状态卡片", &image, 1, 1).expect("可信 PNG 应可内联");
        let debug = format!("{illustration:?}");
        assert!(debug.contains("byte_length"));
        assert!(!debug.contains("iVBOR"));

        let request = CommandRequest {
            args: RString::new(),
            command_name: RString::from("状态"),
            sender_id: RString::from("123"),
            group_id: RString::from("456"),
            raw_event_json: RString::from(r#"{"post_type":"message"}"#),
            sender_nickname: RString::from("测试者"),
            message_id: RString::from("789"),
            timestamp: 0,
        };
        let document = GameDocument::new("角色状态")
            .line("完整文字不能丢失")
            .field("角色", "唐三")
            .command("状态")
            .illustration(illustration);
        let response = response_for(
            &request,
            &document,
            &MessageConfig::default(),
            &IllustrationConfig::default(),
        );
        let segments: Value =
            serde_json::from_str(response.action.segments_json.as_str()).expect("消息段应为 JSON");
        assert_eq!(segments.as_array().map(Vec::len), Some(2));
        assert_eq!(segments[0]["type"], "text");
        assert!(
            segments[0]["data"]["text"]
                .as_str()
                .is_some_and(|text| text.contains("完整文字不能丢失"))
        );
        assert_eq!(segments[1]["type"], "image");
        let encoded = segments[1]["data"]["file"]
            .as_str()
            .and_then(|value| value.strip_prefix("base64://"))
            .expect("OneBot 图片应使用 base64:// 来源");
        assert_eq!(STANDARD.decode(encoded).expect("Base64 应有效"), image);
    }

    #[test]
    fn qq_official_inline_image_falls_back_to_complete_markdown_text() {
        use abi_stable::std_types::RString;

        let image = STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
            .expect("测试 PNG 应可解码");
        let document = GameDocument::new("角色状态")
            .line("完整文字不能丢失")
            .field("角色", "唐三")
            .command("状态")
            .illustration(
                Illustration::inline_image_bytes("状态卡片", image, 1, 1)
                    .expect("可信 PNG 应可内联"),
            );
        let request = CommandRequest {
            args: RString::new(),
            command_name: RString::from("状态"),
            sender_id: RString::from("openid-user"),
            group_id: RString::from("group-openid"),
            raw_event_json: RString::from(r#"{"qqbot_payload":{"id":"message-id"}}"#),
            sender_nickname: RString::from("测试者"),
            message_id: RString::from("message-id"),
            timestamp: 0,
        };
        let response = response_for(
            &request,
            &document,
            &MessageConfig::default(),
            &IllustrationConfig::default(),
        );
        let segments: Value =
            serde_json::from_str(response.action.segments_json.as_str()).expect("消息段应为 JSON");
        assert_eq!(segments.as_array().map(Vec::len), Some(1));
        assert_eq!(segments[0]["type"], "markdown");
        let content = segments[0]["data"]["content"]
            .as_str()
            .expect("Markdown 正文应存在");
        for expected in ["角色状态", "完整文字不能丢失", "唐三", "状态"] {
            assert!(content.contains(expected));
        }
        assert!(!response.action.segments_json.contains("base64://"));
        assert!(!content.contains("!["));
    }

    #[test]
    fn qq_official_independent_image_only_uses_direct_inline_source() {
        let inline = GameDocument::new("状态").illustration(
            Illustration::inline_image_bytes("状态卡片", b"GIF89a", 1, 1)
                .expect("测试 GIF 应可内联"),
        );
        assert!(qq_official_direct_inline_image(&inline, &IllustrationConfig::default()).is_some());

        let https = GameDocument::new("状态").illustration(
            Illustration::https("状态卡片", "https://media.example.com/status.webp", 1, 1)
                .expect("公网图片应有效"),
        );
        assert!(qq_official_direct_inline_image(&https, &IllustrationConfig::default()).is_none());

        let remote = GameDocument::new("状态").illustration(
            Illustration::remote_asset("状态卡片", "cards/status.webp", 1, 1)
                .expect("远程资源键应有效"),
        );
        let remote_config = IllustrationConfig {
            enabled: true,
            mode: IllustrationMode::Remote,
            remote_base_url: "https://media.example.com/douluo".to_string(),
            ..IllustrationConfig::default()
        };
        assert!(qq_official_direct_inline_image(&remote, &remote_config).is_none());

        let disabled = IllustrationConfig {
            enabled: false,
            ..IllustrationConfig::default()
        };
        assert!(qq_official_direct_inline_image(&inline, &disabled).is_none());
    }

    #[test]
    fn official_routes_never_receive_inline_base64() {
        use abi_stable::std_types::RString;

        let image = STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
            .expect("测试 PNG 应可解码");
        let routes = [
            r#"{"qqbot_payload":{"event_type":"GROUP_AT_MESSAGE_CREATE","group_openid":"group-openid"}}"#,
            r#"{"qqbot_payload":{"event_type":"C2C_MESSAGE_CREATE","openid":"user-openid"}}"#,
            r#"{"qqbot_payload":{"event_type":"AT_MESSAGE_CREATE","channel_id":"channel-id","guild_id":"guild-id"}}"#,
            r#"{"qqbot_payload":{"event_type":"DIRECT_MESSAGE_CREATE","channel_id":"dms-id","guild_id":"guild-id"}}"#,
        ];

        for raw_event_json in routes {
            let request = CommandRequest {
                args: RString::new(),
                command_name: RString::from("状态"),
                sender_id: RString::from("user-openid"),
                group_id: RString::new(),
                raw_event_json: RString::from(raw_event_json),
                sender_nickname: RString::from("测试者"),
                message_id: RString::from("message-id"),
                timestamp: 0,
            };
            let document = GameDocument::new("角色状态")
                .line("完整文字不能丢失")
                .illustration(
                    Illustration::inline_image_bytes("状态卡片", &image, 1, 1)
                        .expect("可信 PNG 应可内联"),
                );
            let response = response_for(
                &request,
                &document,
                &MessageConfig::default(),
                &IllustrationConfig::default(),
            );
            assert!(response.action.segments_json.contains("完整文字不能丢失"));
            assert!(!response.action.segments_json.contains("base64://"));
            let segments: Value = serde_json::from_str(response.action.segments_json.as_str())
                .expect("消息段应为 JSON");
            assert_eq!(segments.as_array().map(Vec::len), Some(1));
        }
    }

    #[test]
    fn onebot_private_reply_keeps_text_before_inline_image() {
        use abi_stable::std_types::RString;

        let request = CommandRequest {
            args: RString::new(),
            command_name: RString::from("状态"),
            sender_id: RString::from("123"),
            group_id: RString::new(),
            raw_event_json: RString::from(r#"{"post_type":"message","message_type":"private"}"#),
            sender_nickname: RString::from("测试者"),
            message_id: RString::from("789"),
            timestamp: 0,
        };
        let document = GameDocument::new("角色状态").line("私聊正文").illustration(
            Illustration::inline_image_bytes("状态卡片", b"GIF89a", 1, 1)
                .expect("测试 GIF 应可内联"),
        );
        let response = response_for(
            &request,
            &document,
            &MessageConfig::default(),
            &IllustrationConfig::default(),
        );
        let segments: Value =
            serde_json::from_str(response.action.segments_json.as_str()).expect("消息段应为 JSON");
        assert_eq!(segments[0]["type"], "text");
        assert!(
            segments[0]["data"]["text"]
                .as_str()
                .is_some_and(|text| text.contains("私聊正文"))
        );
        assert_eq!(segments[1]["type"], "image");
        assert!(
            segments[1]["data"]["file"]
                .as_str()
                .is_some_and(|file| file.starts_with("base64://"))
        );
    }

    #[test]
    fn inline_image_rejects_unknown_or_oversized_content() {
        assert!(Illustration::inline_image_bytes("伪图片", b"not an image", 1, 1).is_err());
        let mut oversized = vec![0_u8; MAX_INLINE_IMAGE_BYTES + 1];
        oversized[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        assert!(Illustration::inline_image_bytes("过大图片", oversized, 1, 1).is_err());
    }

    #[test]
    fn official_markdown_never_mixes_independent_image_segment() {
        use abi_stable::std_types::RString;

        let request = CommandRequest {
            args: RString::new(),
            command_name: RString::from("位置"),
            sender_id: RString::from("openid-user"),
            group_id: RString::from("group-openid"),
            raw_event_json: RString::from(r#"{"qqbot_payload":{"id":"message-id"}}"#),
            sender_nickname: RString::from("测试者"),
            message_id: RString::from("message-id"),
            timestamp: 0,
        };
        let document = GameDocument::new("当前位置").illustration(
            Illustration::remote_asset("圣魂村", "maps/holy-soul-village.webp", 640, 360)
                .expect("资源键应有效"),
        );
        let illustrations = IllustrationConfig {
            enabled: true,
            mode: IllustrationMode::Remote,
            remote_base_url: "https://media.example.com/douluo".to_string(),
            ..IllustrationConfig::default()
        };
        let response = response_for(
            &request,
            &document,
            &MessageConfig::default(),
            &illustrations,
        );
        let segments: Value =
            serde_json::from_str(response.action.segments_json.as_str()).expect("消息段应为 JSON");
        assert_eq!(segments.as_array().map(Vec::len), Some(1));
        assert_eq!(segments[0]["type"], "markdown");
        assert!(
            segments[0]["data"]["content"]
                .as_str()
                .is_some_and(|content| content.contains("/media/maps/holy-soul-village.webp"))
        );
    }
}
