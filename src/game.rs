use abi_stable_host_api::CommandRequest;

use crate::assets::IllustrationAssets;
use crate::catalog;
use crate::config::{IllustrationMode, PluginConfig};
use crate::message::{GameDocument, Illustration, detect_protocol};
use crate::store::{IdentityKey, PlayerStatus, Store};

#[derive(Clone, Debug)]
pub struct GameService {
    store: Store,
    config: PluginConfig,
    illustration_assets: IllustrationAssets,
}

impl GameService {
    pub(crate) fn with_assets(
        store: Store,
        config: PluginConfig,
        illustration_assets: IllustrationAssets,
    ) -> Self {
        Self {
            store,
            config,
            illustration_assets,
        }
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
            .illustration_if(self.asset_illustration("map", "圣魂村", "cover"))
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
            .illustration_if(self.asset_illustration("map", "圣魂村", "cover")))
    }

    pub fn awaken(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let key = self.identity_key(req);
        let wuhun = self.store.awaken_wuhun(&key)?;
        let illustration = self.wuhun_illustration(&wuhun.name);
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
        Ok(self.status_document(player))
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

    fn status_document(&self, player: PlayerStatus) -> GameDocument {
        let illustration = player
            .wuhun_name
            .as_deref()
            .and_then(|name| self.wuhun_illustration(name));
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

    fn wuhun_illustration(&self, name: &str) -> Option<Illustration> {
        self.asset_illustration("wuhun", name, "portrait")
    }

    fn asset_illustration(
        &self,
        entity_type: &str,
        entity_key: &str,
        media_role: &str,
    ) -> Option<Illustration> {
        let binding = catalog::binding(entity_type, entity_key, media_role)?;
        let width = binding.display.width;
        let height = binding.display.height;
        match self.config.illustrations.mode {
            IllustrationMode::Direct => {
                self.illustration_assets
                    .get(&binding.asset_key)
                    .and_then(|bytes| {
                        Illustration::inline_image_arc(&binding.alt, bytes, width, height).ok()
                    })
                    .or_else(|| {
                        binding.direct_url.as_deref().and_then(|url| {
                            Illustration::https(&binding.alt, url, width, height).ok()
                        })
                    })
            }
            IllustrationMode::Remote => {
                Illustration::remote_asset(&binding.alt, &binding.asset_key, width, height).ok()
            }
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
        let service = GameService::with_assets(store, config, IllustrationAssets::default());
        assert!(service.menu().has_illustration());
        assert!(catalog::binding("wuhun", "独狼", "portrait").is_some());
        assert!(catalog::binding("wuhun", "不存在", "portrait").is_none());

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
                .is_some_and(|content| {
                    content.contains("/media/maps/holy-soul-village/cover.webp")
                })
        );
    }

    #[test]
    fn direct_mode_uses_preloaded_asset_bytes() {
        let directory = tempfile::tempdir().expect("临时目录应创建");
        let image = directory
            .path()
            .join("douluo-game/assets/maps/holy-soul-village/cover.webp");
        std::fs::create_dir_all(image.parent().expect("父目录")).expect("目录应创建");
        std::fs::write(&image, b"RIFF\x04\x00\x00\x00WEBP").expect("测试资源应写入");
        let store = Store::initialize(directory.path(), &crate::config::DatabaseConfig::default())
            .expect("数据库应初始化");
        let config = PluginConfig::default();
        let assets = IllustrationAssets::load(directory.path(), &config.illustrations)
            .expect("本地资源应预加载");
        let service = GameService::with_assets(store, config, assets);
        let request = CommandRequest {
            args: abi_stable::std_types::RString::new(),
            command_name: abi_stable::std_types::RString::from("斗罗系统"),
            sender_id: abi_stable::std_types::RString::from("user"),
            group_id: abi_stable::std_types::RString::from("group"),
            raw_event_json: abi_stable::std_types::RString::from(r#"{"post_type":"message"}"#),
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
        assert_eq!(segments[0]["type"], "text");
        assert_eq!(segments[1]["type"], "image");
        assert!(
            segments[1]["data"]["file"]
                .as_str()
                .is_some_and(|source| source.starts_with("base64://"))
        );
    }

    #[test]
    fn direct_mode_does_not_fall_back_to_remote_base_url() {
        let directory = tempfile::tempdir().expect("临时目录应创建");
        let store = Store::initialize(directory.path(), &crate::config::DatabaseConfig::default())
            .expect("数据库应初始化");
        let mut config = PluginConfig::default();
        config.illustrations.remote_base_url = "https://media.example.com/douluo".to_string();
        let assets = IllustrationAssets::load(directory.path(), &config.illustrations)
            .expect("缺少本地目录时应降级");
        let service = GameService::with_assets(store, config, assets);
        assert!(!service.menu().has_illustration());
    }
}
