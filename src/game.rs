use abi_stable_host_api::CommandRequest;

use crate::config::PluginConfig;
use crate::message::{GameDocument, Illustration, detect_protocol};
use crate::store::{IdentityKey, PlayerStatus, Store};

#[derive(Clone, Debug)]
pub struct GameService {
    store: Store,
    config: PluginConfig,
}

impl GameService {
    pub fn new(store: Store, config: PluginConfig) -> Self {
        Self { store, config }
    }

    pub fn message_config(&self) -> &crate::config::MessageConfig {
        &self.config.messages
    }

    pub fn illustration_config(&self) -> &crate::config::IllustrationConfig {
        &self.config.illustrations
    }

    pub fn menu(&self) -> GameDocument {
        GameDocument::new("斗罗系统")
            .line("欢迎来到斗罗大陆。先创建角色，再觉醒属于你的武魂。")
            .command("开始穿越 <角色名> <男|女>")
            .command("武魂觉醒")
            .command("状态")
            .illustration_if(remote_illustration(
                "斗罗大陆主菜单",
                "ui/menu/cover.webp",
                1200,
                675,
            ))
            .notice("命令前缀、群聊 @ 和回复入口由 QimenBot 宿主配置决定")
    }

    pub fn register(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let (name, gender) = parse_registration_args(
            req.args.as_str(),
            self.config.messages.legacy_hyphen_arguments,
        )?;
        let name_length = name.chars().count();
        if !(2..=self.config.identity.max_character_name_chars).contains(&name_length) {
            return Err(format!(
                "角色名长度必须在 2 到 {} 个字符之间",
                self.config.identity.max_character_name_chars
            ));
        }
        if !matches!(gender, "男" | "女") {
            return Err("性别只能填写“男”或“女”".to_string());
        }
        let key = self.identity_key(req);
        let player = self.store.register_player(&key, name, gender)?;
        Ok(GameDocument::new("穿越成功")
            .field("角色", player.name)
            .field("性别", player.gender)
            .field("出生地", player.map_name)
            .field("生命", format!("{}/{}", player.hp, player.max_hp))
            .field(
                "魂力",
                format!("{}/{}", player.soul_power, player.max_soul_power),
            )
            .command("武魂觉醒")
            .command("状态")
            .illustration_if(remote_illustration(
                "圣魂村",
                "maps/holy-soul-village/cover.webp",
                1200,
                675,
            )))
    }

    pub fn awaken(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let key = self.identity_key(req);
        let wuhun = self.store.awaken_wuhun(&key)?;
        let illustration = wuhun_illustration(&wuhun.name);
        Ok(GameDocument::new("武魂觉醒")
            .line("觉醒仪式完成，你感受到一股崭新的力量。")
            .field("武魂", wuhun.name)
            .field("类别", wuhun.category)
            .field("形态", wuhun.form)
            .field("描述", wuhun.description)
            .illustration_if(illustration)
            .command("状态"))
    }

    pub fn status(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let key = self.identity_key(req);
        let player = self
            .store
            .player_status(&key)?
            .ok_or_else(|| "你还没有角色，请先使用“开始穿越 角色名 性别”".to_string())?;
        Ok(status_document(player))
    }

    fn identity_key<'a>(&'a self, req: &'a CommandRequest) -> IdentityKey<'a> {
        IdentityKey {
            protocol: detect_protocol(req.raw_event_json.as_str()),
            namespace: &self.config.identity.namespace,
            // 同一 OneBot 用户在群聊和私聊中必须命中同一份角色存档。
            subject_kind: "user",
            subject_id: req.sender_id.as_str(),
        }
    }
}

fn parse_registration_args(args: &str, legacy_hyphen: bool) -> Result<(&str, &str), String> {
    let args = args.trim();
    let parts: Vec<&str> = args.split_whitespace().collect();
    if let [name, gender] = parts.as_slice() {
        return Ok((name.trim(), gender.trim()));
    }
    if legacy_hyphen
        && let Some((name, gender)) = args.rsplit_once('-')
        && !name.trim().is_empty()
        && !gender.trim().is_empty()
    {
        return Ok((name.trim(), gender.trim()));
    }
    Err("用法：开始穿越 <角色名> <男|女>；也兼容旧格式“角色名-性别”".to_string())
}

fn status_document(player: PlayerStatus) -> GameDocument {
    let illustration = player.wuhun_name.as_deref().and_then(wuhun_illustration);
    let wuhun = match (player.wuhun_name, player.wuhun_category) {
        (Some(name), Some(category)) => format!("{name}（{category}）"),
        _ => "尚未觉醒".to_string(),
    };
    GameDocument::new("角色状态")
        .field("角色", player.name)
        .field("性别", player.gender)
        .field("境界", format!("{} 级魂士", player.level))
        .field("经验", player.exp.to_string())
        .field("生命", format!("{}/{}", player.hp, player.max_hp))
        .field(
            "魂力",
            format!("{}/{}", player.soul_power, player.max_soul_power),
        )
        .field("武魂", wuhun)
        .field("位置", player.map_name)
        .field("转生", format!("第 {} 世", player.life_count))
        .field("状态", player.state)
        .illustration_if(illustration)
}

fn remote_illustration(
    alt: &str,
    asset_key: &str,
    width: u16,
    height: u16,
) -> Option<Illustration> {
    Illustration::remote_asset(alt, asset_key, width, height).ok()
}

fn wuhun_illustration(name: &str) -> Option<Illustration> {
    let asset_key = match name {
        "独狼" => "wuhun/lone-wolf/portrait.webp",
        "萝卜" => "wuhun/carrot/portrait.webp",
        "镰刀" => "wuhun/sickle/portrait.webp",
        _ => return None,
    };
    remote_illustration(name, asset_key, 640, 640)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_space_and_legacy_hyphen_arguments() {
        assert_eq!(
            parse_registration_args("唐小三 男", true),
            Ok(("唐小三", "男"))
        );
        assert_eq!(
            parse_registration_args("唐-小三-男", true),
            Ok(("唐-小三", "男"))
        );
        assert!(parse_registration_args("唐小三-男", false).is_err());
    }

    #[test]
    fn menu_and_wuhun_documents_carry_stable_asset_keys() {
        let directory = tempfile::tempdir().expect("临时目录应创建");
        let store = Store::initialize(directory.path(), &crate::config::DatabaseConfig::default())
            .expect("数据库应初始化");
        let mut config = PluginConfig::default();
        config.illustrations.mode = crate::config::IllustrationMode::Remote;
        config.illustrations.remote_base_url = "https://media.example.com/douluo".to_string();
        let service = GameService::new(store, config);
        assert!(service.menu().has_illustration());
        assert!(wuhun_illustration("独狼").is_some());
        assert!(wuhun_illustration("不存在").is_none());

        let request = CommandRequest {
            args: abi_stable::std_types::RString::new(),
            command_name: abi_stable::std_types::RString::from("斗罗系统"),
            sender_id: abi_stable::std_types::RString::from("user"),
            group_id: abi_stable::std_types::RString::new(),
            raw_event_json: abi_stable::std_types::RString::from(
                r#"{"qqbot_payload":{"id":"message"}}"#,
            ),
            sender_nickname: abi_stable::std_types::RString::new(),
            message_id: abi_stable::std_types::RString::new(),
            timestamp: 0,
        };
        let response = crate::message::response_for(
            &request,
            &service.menu(),
            service.message_config(),
            service.illustration_config(),
        );
        let segments: serde_json::Value =
            serde_json::from_str(response.action.segments_json.as_str()).expect("消息段应为 JSON");
        assert!(
            segments[0]["data"]["content"]
                .as_str()
                .is_some_and(|content| content.contains("/media/ui/menu/cover.webp"))
        );
    }
}
