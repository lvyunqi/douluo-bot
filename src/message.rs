use abi_stable_host_api::{CommandRequest, CommandResponse, DynamicActionResponse};
use serde_json::{Value, json};

use crate::config::MessageConfig;

/// 领域用例输出的协议无关文档。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GameDocument {
    title: String,
    lines: Vec<String>,
    fields: Vec<(String, String)>,
    commands: Vec<String>,
    notice: Option<String>,
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
    config: &MessageConfig,
) -> CommandResponse {
    let protocol = detect_protocol(req.raw_event_json.as_str());
    let markdown = match protocol {
        Protocol::OneBot11 => config.onebot_markdown,
        Protocol::QqOfficial => config.qq_official_markdown,
    };
    if markdown {
        markdown_response(document)
    } else {
        CommandResponse::text(&render_text(document))
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

pub fn render_markdown(document: &GameDocument) -> String {
    let mut output = vec![format!("# {}", escape_markdown(&document.title))];
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

fn markdown_response(document: &GameDocument) -> CommandResponse {
    let segments = json!([{
        "type": "markdown",
        "data": { "content": render_markdown(document) }
    }]);
    CommandResponse {
        action: DynamicActionResponse::rich_reply(&segments.to_string()),
    }
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
        let output = render_markdown(&document);
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
}
