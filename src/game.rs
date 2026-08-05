use abi_stable_host_api::CommandRequest;

use crate::assets::IllustrationAssets;
use crate::catalog;
use crate::config::{IllustrationMode, PluginConfig};
use crate::message::{GameDocument, Illustration, detect_protocol};
use crate::store::{IdentityKey, PlayerStatus, Store};

const MENU_PAGES: &[MenuPage] = &[
    MenuPage {
        key: "开始",
        title: "开始游戏",
        entries: &[MenuEntry {
            command: "开始穿越 <角色名> <男|女>",
            description: "创建你的斗罗大陆角色",
        }],
    },
    MenuPage {
        key: "角色",
        title: "角色成长",
        entries: &[
            MenuEntry {
                command: "武魂觉醒",
                description: "觉醒第一武魂",
            },
            MenuEntry {
                command: "状态",
                description: "查看角色属性、武魂和位置",
            },
        ],
    },
    MenuPage {
        key: "世界",
        title: "世界探索",
        entries: &[MenuEntry {
            command: "位置",
            description: "查看当前地图",
        }],
    },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MenuEntry {
    command: &'static str,
    description: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MenuPage {
    key: &'static str,
    title: &'static str,
    entries: &'static [MenuEntry],
}

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

    pub fn menu(&self, args: &str) -> Result<GameDocument, String> {
        let page_index = parse_menu_page(args)?;
        let page = MENU_PAGES
            .get(page_index)
            .ok_or_else(|| format!("菜单页码必须在 1 到 {} 之间", MENU_PAGES.len()))?;
        let mut document = GameDocument::new(format!("斗罗系统 · {}", page.title))
            .line("欢迎来到斗罗大陆。先创建角色，再觉醒属于你的武魂。")
            .field("分类", page.title)
            .field("页码", format!("{} / {}", page_index + 1, MENU_PAGES.len()));
        for entry in page.entries {
            document = document.command_help(entry.command, entry.description);
        }
        if page_index > 0 {
            document = document.command(format!("斗罗系统 {}", page_index));
        }
        if page_index + 1 < MENU_PAGES.len() {
            document = document.command(format!("斗罗系统 {}", page_index + 2));
        }
        let illustration = if page_index == 0 {
            self.asset_illustration("map", "圣魂村", "cover")
        } else {
            None
        };
        Ok(document.illustration_if(illustration).notice(
            "输入“斗罗系统 <页码或分类>”翻页；命令前缀、群聊 @ 和回复入口由 QimenBot 宿主配置决定",
        ))
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

    pub fn location(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        if !req.args.as_str().trim().is_empty() {
            return Err("用法：位置".to_string());
        }
        let key = self.identity_key(req);
        let player = self
            .store
            .player_status(&key)?
            .ok_or_else(|| "你还没有角色，请先使用“开始穿越 角色名 性别”".to_string())?;
        let map_name = player.map_name.clone();
        Ok(GameDocument::new(format!("当前位置 · {map_name}"))
            .field("角色", player.name)
            .field("地图", &map_name)
            .field("生命", format!("{}/{}", player.hp, player.max_hp))
            .field(
                "魂力",
                format!("{}/{}", player.soul_power, player.max_soul_power),
            )
            .illustration_if(self.asset_illustration("map", &map_name, "cover"))
            .command("状态"))
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

fn parse_menu_page(args: &str) -> Result<usize, String> {
    let args = args.trim();
    if args.is_empty() {
        return Ok(0);
    }
    let mut parts = args.split_whitespace();
    let token = parts.next().unwrap_or_default();
    if parts.next().is_some() {
        return Err("用法：斗罗系统 [页码|开始|角色|世界]".to_string());
    }
    if let Some(page) = token
        .parse::<usize>()
        .ok()
        .and_then(|page| page.checked_sub(1))
    {
        if page < MENU_PAGES.len() {
            return Ok(page);
        }
        return Err(format!("菜单页码必须在 1 到 {} 之间", MENU_PAGES.len()));
    }
    MENU_PAGES
        .iter()
        .position(|page| page.key == token)
        .ok_or_else(|| "用法：斗罗系统 [页码|开始|角色|世界]".to_string())
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
    fn menu_pages_only_expose_implemented_commands() {
        let first = parse_menu_page("").expect("默认菜单页应有效");
        let second = parse_menu_page("角色").expect("角色分类应有效");
        assert_eq!(first, 0);
        assert_eq!(second, 1);
        assert!(
            MENU_PAGES[first]
                .entries
                .iter()
                .any(|entry| entry.command.starts_with("开始穿越"))
        );
        assert!(
            MENU_PAGES[second]
                .entries
                .iter()
                .any(|entry| entry.command == "状态")
        );
        assert_eq!(parse_menu_page("世界").expect("世界分类应有效"), 2);
        assert!(parse_menu_page("4").is_err());
        assert!(parse_menu_page("角色 多余").is_err());
    }

    #[test]
    fn menu_document_renders_page_navigation_without_future_commands() {
        let directory = tempfile::tempdir().expect("临时目录应创建");
        let store = Store::initialize(directory.path(), &crate::config::DatabaseConfig::default())
            .expect("数据库应初始化");
        let service = GameService::with_assets(
            store,
            PluginConfig::default(),
            IllustrationAssets::default(),
        );
        let first = crate::message::render_text(&service.menu("").expect("第一页应有效"));
        assert!(first.contains("开始穿越 <角色名> <男|女>：创建你的斗罗大陆角色"));
        assert!(first.contains("斗罗系统 2"));
        assert!(!first.contains("地图"));

        let second = crate::message::render_text(&service.menu("2").expect("第二页应有效"));
        assert!(second.contains("武魂觉醒：觉醒第一武魂"));
        assert!(second.contains("状态：查看角色属性、武魂和位置"));
        assert!(second.contains("斗罗系统 1"));
        assert!(second.contains("斗罗系统 3"));

        let third = crate::message::render_text(&service.menu("世界").expect("世界页应有效"));
        assert!(third.contains("位置：查看当前地图"));
        assert!(third.contains("斗罗系统 2"));
    }

    #[test]
    fn location_requires_a_player_and_uses_the_current_map_illustration() {
        let directory = tempfile::tempdir().expect("临时目录应创建");
        let store = Store::initialize(directory.path(), &crate::config::DatabaseConfig::default())
            .expect("数据库应初始化");
        let mut config = PluginConfig::default();
        config.illustrations.mode = crate::config::IllustrationMode::Remote;
        config.illustrations.remote_base_url = "https://media.example.com/douluo".to_string();
        let service = GameService::with_assets(store, config, IllustrationAssets::default());
        let request = CommandRequest {
            args: abi_stable::std_types::RString::new(),
            command_name: abi_stable::std_types::RString::from("位置"),
            sender_id: abi_stable::std_types::RString::from("user-location"),
            group_id: abi_stable::std_types::RString::new(),
            raw_event_json: abi_stable::std_types::RString::from(
                r#"{"post_type":"message","self_id":10001}"#,
            ),
            sender_nickname: abi_stable::std_types::RString::new(),
            message_id: abi_stable::std_types::RString::new(),
            timestamp: 0,
        };
        assert!(service.location(&request).is_err());
        service
            .register(&CommandRequest {
                args: abi_stable::std_types::RString::from("唐小三 男"),
                command_name: abi_stable::std_types::RString::from("开始穿越"),
                sender_id: abi_stable::std_types::RString::from("user-location"),
                group_id: abi_stable::std_types::RString::new(),
                raw_event_json: abi_stable::std_types::RString::from(
                    r#"{"post_type":"message","self_id":10001}"#,
                ),
                sender_nickname: abi_stable::std_types::RString::new(),
                message_id: abi_stable::std_types::RString::new(),
                timestamp: 0,
            })
            .expect("角色应创建");
        let location = service.location(&request).expect("位置应可查询");
        let text = crate::message::render_text(&location);
        assert!(text.contains("当前位置 · 圣魂村"));
        assert!(text.contains("角色：唐小三"));
        assert!(location.has_illustration());

        let response = crate::message::response_for(
            &request,
            &location,
            service.message_config(),
            service.illustration_config(),
        );
        let segments: serde_json::Value =
            serde_json::from_str(response.action.segments_json.as_str()).expect("消息段应为 JSON");
        assert_eq!(
            segments[1]["data"]["file"],
            "https://media.example.com/douluo/media/maps/holy-soul-village/cover.webp"
        );

        let direct = GameService::with_assets(
            Store::initialize(directory.path(), &crate::config::DatabaseConfig::default())
                .expect("数据库应重新打开"),
            PluginConfig::default(),
            IllustrationAssets::default(),
        );
        let direct_location = direct.location(&request).expect("direct 位置应可查询");
        assert!(!direct_location.has_illustration());
        assert!(crate::message::render_text(&direct_location).contains("圣魂村"));

        let invalid_request = CommandRequest {
            args: abi_stable::std_types::RString::from("多余参数"),
            command_name: abi_stable::std_types::RString::from("位置"),
            sender_id: abi_stable::std_types::RString::from("user-location"),
            group_id: abi_stable::std_types::RString::new(),
            raw_event_json: abi_stable::std_types::RString::from(
                r#"{"post_type":"message","self_id":10001}"#,
            ),
            sender_nickname: abi_stable::std_types::RString::new(),
            message_id: abi_stable::std_types::RString::new(),
            timestamp: 0,
        };
        assert_eq!(
            service.location(&invalid_request),
            Err("用法：位置".to_string())
        );
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
        assert!(
            service
                .menu("")
                .expect("默认菜单页应有效")
                .has_illustration()
        );
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
            &service.menu("").expect("默认菜单页应有效"),
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
            &service.menu("").expect("默认菜单页应有效"),
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
        assert!(
            !service
                .menu("")
                .expect("默认菜单页应有效")
                .has_illustration()
        );
    }
}
