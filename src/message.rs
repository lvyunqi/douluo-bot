use abi_stable_host_api::{CommandRequest, CommandResponse, DynamicActionResponse};
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

/// 可被 QQ 官方 Markdown 拉取、也可由 OneBot 图片段发送的公网插图。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Illustration {
    alt: String,
    source: IllustrationSource,
    width: u16,
    height: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum IllustrationSource {
    DirectHttps(String),
    RemoteAsset(String),
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

    fn resolve_url(&self, config: &IllustrationConfig) -> Option<String> {
        if !config.enabled {
            return None;
        }
        match (&self.source, config.mode) {
            (IllustrationSource::DirectHttps(url), IllustrationMode::Direct) => Some(url.clone()),
            (IllustrationSource::RemoteAsset(key), IllustrationMode::Remote) => {
                config.remote_asset_url(key)
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
    let illustration_url = document
        .illustration
        .as_ref()
        .and_then(|illustration| illustration.resolve_url(illustrations));
    match protocol {
        Protocol::QqOfficial if markdown => {
            markdown_response(document, illustration_url.as_deref())
        }
        Protocol::OneBot11 if illustration_url.is_some() => {
            onebot_illustrated_response(document, markdown, illustration_url.as_deref())
        }
        _ if markdown => markdown_response(document, None),
        _ => CommandResponse::text(&render_text(document)),
    }
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
    illustration_url: Option<&str>,
) -> CommandResponse {
    let Some(illustration_url) = illustration_url else {
        return CommandResponse::text(&render_text(document));
    };
    let body = if markdown {
        json!({"type": "markdown", "data": {"content": render_markdown(document, None)}})
    } else {
        json!({"type": "text", "data": {"text": render_text(document)}})
    };
    let segments = json!([
        body,
        {"type": "image", "data": {"file": illustration_url}}
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
        };
        assert_eq!(
            illustration.resolve_url(&config).as_deref(),
            Some("https://media.example.com/douluo/media/wuhun/lone-wolf/portrait.webp")
        );
        assert!(Illustration::remote_asset("非法", "../secret.png", 10, 10).is_err());
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
