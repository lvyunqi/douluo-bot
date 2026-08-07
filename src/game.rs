use abi_stable_host_api::CommandRequest;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::assets::IllustrationAssets;
use crate::catalog;
use crate::config::{AuthorizationMode, IllustrationMode, PluginConfig};
use crate::context::resolve_conversation_context;
use crate::identity::{
    ResolvedIdentity, parse_target_subject_id, resolve_identity, resolve_protocol,
    resolve_target_mention,
};
use crate::message::{GameDocument, Illustration};
use crate::store::{
    AuthorizedContextChange, BattleActionReceipt, BattleEventRecord, BattleLog,
    BattleSkillEffectRecord, BattleSnapshot, DailyCheckinInput, DailyCheckinResult, GOLD_SOUL_COIN,
    IdentityKey, LegacyClaimActor, LegacyClaimResult, LegacyIdentityState, MAX_SKILL_LEVEL,
    MAX_SKILL_PROFICIENCY, MapExit, MapRecord, MapTravelReceipt, OperationLogInput, PlayerStatus,
    QuestActionReceipt, QuestListEntry, SkillDamageModifierRecord, SkillEffectRecord,
    SkillLoadoutReceipt, SkillPage, SoulBeastPage, SoulRingAbsorbReceipt, SoulRingPage, Store,
    WuhunToggleReceipt, experience_progress, skill_damage_percent, skill_proficiency_threshold,
};

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
                command: "开武魂",
                description: "开启当前武魂并进入战斗形态",
            },
            MenuEntry {
                command: "关武魂",
                description: "关闭当前武魂并退出战斗形态",
            },
            MenuEntry {
                command: "状态",
                description: "查看角色属性、武魂和位置",
            },
            MenuEntry {
                command: "技能",
                description: "查看已学习魂技和魂力消耗",
            },
            MenuEntry {
                command: "装备魂技 <魂技>",
                description: "装备已学习魂技并加入战斗可用列表",
            },
            MenuEntry {
                command: "卸下魂技 <魂技>",
                description: "卸下已装备魂技，至少保留一个可用魂技",
            },
            MenuEntry {
                command: "魂环",
                description: "查看已吸收魂环和待吸收魂环",
            },
            MenuEntry {
                command: "签到",
                description: "领取每日经验和金魂币",
            },
            MenuEntry {
                command: "钱包",
                description: "查看金魂币余额",
            },
        ],
    },
    MenuPage {
        key: "世界",
        title: "世界探索",
        entries: &[
            MenuEntry {
                command: "位置",
                description: "查看当前地图、出口和区域信息",
            },
            MenuEntry {
                command: "地图列表 [页码]",
                description: "分页查看地图和等级要求",
            },
            MenuEntry {
                command: "向 <上|下|左|右>",
                description: "沿当前地图出口移动",
            },
            MenuEntry {
                command: "传送 [地图]",
                description: "从传送阵前往可达地图",
            },
            MenuEntry {
                command: "掉落 [页码]",
                description: "查看当前地图可拾取的地面掉落",
            },
            MenuEntry {
                command: "拾取 <掉落ID>",
                description: "拾取当前地图的一整堆物品",
            },
        ],
    },
    MenuPage {
        key: "经济",
        title: "NPC 与物品",
        entries: &[
            MenuEntry {
                command: "NPC",
                description: "查看当前地图的 NPC",
            },
            MenuEntry {
                command: "对话 <NPC>",
                description: "与当前地图的 NPC 对话",
            },
            MenuEntry {
                command: "商店 [页码]",
                description: "查看已对话商人的商品",
            },
            MenuEntry {
                command: "背包 [页码]",
                description: "查看随身物品",
            },
            MenuEntry {
                command: "购买 <物品> [数量]",
                description: "从当前商店购买物品",
            },
            MenuEntry {
                command: "出售 <物品> [数量]",
                description: "向当前商店出售物品",
            },
            MenuEntry {
                command: "使用 <物品>",
                description: "使用背包中的消耗品",
            },
            MenuEntry {
                command: "转账 <用户ID> <金额>",
                description: "向同一 Bot 身份域的玩家转账",
            },
            MenuEntry {
                command: "发送物品 <用户ID> <物品> [数量]",
                description: "向同一 Bot 身份域的玩家赠送物品",
            },
        ],
    },
    MenuPage {
        key: "任务",
        title: "任务与成长",
        entries: &[
            MenuEntry {
                command: "任务 [页码]",
                description: "查看当前地图可接取的任务",
            },
            MenuEntry {
                command: "接取任务 <任务>",
                description: "接取一项可用任务",
            },
            MenuEntry {
                command: "任务进度 [任务]",
                description: "查看进行中的任务进度",
            },
            MenuEntry {
                command: "提交任务 <任务>",
                description: "提交已完成任务并领取奖励",
            },
            MenuEntry {
                command: "放弃任务 <任务>",
                description: "放弃进行中的任务",
            },
        ],
    },
    MenuPage {
        key: "战斗",
        title: "魂兽战斗",
        entries: &[
            MenuEntry {
                command: "魂兽 [页码]",
                description: "查看当前地图可挑战的魂兽",
            },
            MenuEntry {
                command: "挑战 <魂兽>",
                description: "发起一场魂兽挑战",
            },
            MenuEntry {
                command: "攻击",
                description: "按武魂修正进行普通攻击并承受魂兽反击",
            },
            MenuEntry {
                command: "吸收魂环 <魂兽>",
                description: "吸收击杀魂兽留下的魂环并解锁魂技",
            },
            MenuEntry {
                command: "逃跑",
                description: "尝试结束当前战斗",
            },
            MenuEntry {
                command: "战斗状态",
                description: "查看当前战斗快照",
            },
            MenuEntry {
                command: "战斗日志",
                description: "查看最近战斗事件",
            },
        ],
    },
];

const MAP_PAGE_SIZE: usize = 5;

const CHECKIN_CURRENCY_CODE: &str = GOLD_SOUL_COIN;
const CHECKIN_CURRENCY_NAME: &str = "金魂币";
const TRANSFER_USAGE: &str = "转账 <用户ID> <金额>；也可使用“转账 @用户 <金额>”";
const GIFT_USAGE: &str =
    "发送物品 <用户ID> <物品名> [数量]；也可使用“发送物品 @用户 <物品名> [数量]”";
const CHECKIN_EXP_REWARDS: [i64; 7] = [60, 70, 80, 90, 100, 110, 150];
const SECONDS_PER_DAY: i64 = 86_400;
const BEIJING_04_OFFSET_SECONDS: i64 = 4 * 3_600;

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

    pub fn record_operation(&self, req: &CommandRequest, outcome: &str) -> Result<(), String> {
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let details = operation_details(req, identity.protocol);
        self.store
            .append_operation(
                &key,
                req.command_name.as_str(),
                outcome,
                req.message_id.as_str(),
                &details,
            )
            .map(|_| ())
    }

    pub fn ensure_context_authorized(&self, req: &CommandRequest) -> Result<(), String> {
        if self.config.authorization.mode == AuthorizationMode::AllowAll {
            return Ok(());
        }
        let protocol = resolve_protocol(req)?;
        let context = resolve_conversation_context(req, protocol)?;
        let Some((context_kind, context_id)) = context.authorization_target() else {
            return Ok(());
        };
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        if self.store.is_authorized(&key, context_kind, context_id)? {
            Ok(())
        } else {
            Err(format!(
                "当前{}尚未授权使用斗罗大陆游戏，请联系机器人所有者",
                if context_kind == "channel" {
                    "频道"
                } else {
                    "群聊"
                }
            ))
        }
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
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let details = operation_details(req, identity.protocol);
        let operation = successful_operation(req, &details);
        let player =
            self.store
                .register_player_with_operation(&key, name, gender, Some(&operation))?;
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
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let details = operation_details(req, identity.protocol);
        let operation = successful_operation(req, &details);
        let wuhun = self
            .store
            .awaken_wuhun_with_operation(&key, Some(&operation))?;
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

    pub fn skills(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        if !req.args.as_str().trim().is_empty() {
            return Err("用法：技能".to_string());
        }
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let page = self.store.skills_page(&key)?;
        Ok(self.skills_document(page))
    }

    pub fn skill_detail(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let skill_name = parse_required_catalog_name(req.args.as_str(), "技能详情 <魂技>")?;
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let skill = self
            .store
            .skill_detail(&key, skill_name)?
            .ok_or_else(|| format!("你尚未学习魂技“{skill_name}”"))?;
        let damage_percent = skill_damage_percent(skill.level)?;
        let mut document = GameDocument::new(format!("魂技详情 · {}", skill.skill.name))
            .field("类型", skill.skill.skill_type.clone())
            .field("魂环", format!("第{}魂技", skill.skill.ring_index))
            .field("等级", skill.level.to_string())
            .field(
                "熟练度",
                skill_proficiency_label(skill.level, skill.proficiency),
            )
            .field(
                "装备状态",
                if skill.equipped {
                    "已装备"
                } else {
                    "未装备"
                },
            )
            .field("魂力消耗", skill.skill.soul_power_cost.to_string())
            .field("冷却", format!("{} 回合", skill.skill.cooldown_rounds))
            .field("基础伤害", skill.skill.base_damage.to_string())
            .field("等级伤害倍率", format!("{damage_percent}%"));
        for effect in &skill.effects {
            document = document.field("附加效果", skill_effect_label(effect));
        }
        Ok(document
            .line(skill.skill.description)
            .command("技能")
            .command(format!("释放技能 {}", skill.skill.name)))
    }

    pub fn equip_skill(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        self.set_skill_equipped(req, true)
    }

    pub fn unequip_skill(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        self.set_skill_equipped(req, false)
    }

    fn set_skill_equipped(
        &self,
        req: &CommandRequest,
        equipped: bool,
    ) -> Result<GameDocument, String> {
        let skill_name = parse_required_catalog_name(
            req.args.as_str(),
            if equipped {
                "装备魂技 <魂技>"
            } else {
                "卸下魂技 <魂技>"
            },
        )?;
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let details = operation_details(req, identity.protocol);
        let operation = successful_operation(req, &details);
        let receipt = if equipped {
            self.store
                .equip_skill_with_operation(&key, skill_name, &operation)?
        } else {
            self.store
                .unequip_skill_with_operation(&key, skill_name, &operation)?
        };
        Ok(self.skill_loadout_document(receipt))
    }

    fn skill_loadout_document(&self, receipt: SkillLoadoutReceipt) -> GameDocument {
        GameDocument::new(if receipt.replayed {
            "魂技装备回执"
        } else if receipt.equipped {
            "魂技装备成功"
        } else {
            "魂技卸下成功"
        })
        .field("魂技", receipt.skill.name.clone())
        .field(
            "状态",
            if receipt.equipped {
                "已装备"
            } else {
                "未装备"
            },
        )
        .field(
            "装备位",
            format!("{}/{}", receipt.equipped_count, receipt.capacity),
        )
        .notice(if receipt.replayed {
            "检测到相同消息的重复请求，已返回原装备回执，未重复变更状态"
        } else {
            "魂技装备状态、操作日志和不可变装备事件已在同一事务完成"
        })
        .command("技能")
        .command(format!(
            "{}魂技 {}",
            if receipt.equipped { "卸下" } else { "装备" },
            receipt.skill.name
        ))
    }

    pub fn soul_rings(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        if !req.args.as_str().trim().is_empty() {
            return Err("用法：魂环".to_string());
        }
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let page = self.store.soul_rings_page(&key)?;
        Ok(self.soul_rings_document(page))
    }

    pub fn absorb_soul_ring(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let soul_beast_name = parse_required_catalog_name(req.args.as_str(), "吸收魂环 <魂兽>")?;
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let details = operation_details(req, identity.protocol);
        let operation = successful_operation(req, &details);
        let receipt =
            self.store
                .absorb_soul_ring_with_operation(&key, soul_beast_name, &operation)?;
        Ok(self.absorb_soul_ring_document(receipt))
    }

    fn soul_rings_document(&self, page: SoulRingPage) -> GameDocument {
        let mut document = GameDocument::new("魂环列表").field(
            "魂环槽位",
            format!("{}/{}", page.rings.len(), page.ring_capacity),
        );
        if page.rings.is_empty() {
            document = document.line("你还没有吸收任何魂环");
        } else {
            for ring in &page.rings {
                document = document
                    .line(format!(
                        "第{}魂环 · {} · {} · {}年",
                        ring.ring_index,
                        ring.ring.name,
                        soul_ring_color_label(&ring.ring.color),
                        ring.ring.age
                    ))
                    .line(format!("魂技：{}", ring.ring.skill.name));
            }
        }
        if page.pending.is_empty() {
            document = document.line("当前没有待吸收魂环");
        } else {
            document = document.line("待吸收魂环：");
            for drop in &page.pending {
                document = document
                    .line(format!(
                        "{} · {} · {}年",
                        drop.ring.soul_beast_name, drop.ring.name, drop.ring.age
                    ))
                    .command(format!("吸收魂环 {}", drop.ring.soul_beast_name));
            }
        }
        document.command("技能").command("状态")
    }

    fn absorb_soul_ring_document(&self, receipt: SoulRingAbsorbReceipt) -> GameDocument {
        GameDocument::new(if receipt.replayed {
            "魂环吸收回执"
        } else {
            "魂环吸收成功"
        })
        .field("魂环", receipt.ring.ring.name)
        .field("魂兽", receipt.ring.ring.soul_beast_name)
        .field("年限", format!("{}年", receipt.ring.ring.age))
        .field("品质", soul_ring_color_label(&receipt.ring.ring.color))
        .field("魂技", receipt.skill.name)
        .field("魂环槽位", format!("第{}魂环", receipt.ring.ring_index))
        .notice(if receipt.replayed {
            "检测到相同消息的重复请求，已返回原吸收回执，未重复占用魂环槽位"
        } else {
            "魂环、魂技绑定、操作日志和吸收事件已在同一事务完成"
        })
        .command("魂环")
        .command("技能")
    }

    fn skills_document(&self, page: SkillPage) -> GameDocument {
        let mut document = GameDocument::new("魂技列表").field(
            "魂力",
            format!("{}/{}", page.soul_power, page.max_soul_power),
        );
        if page.entries.is_empty() {
            return document
                .line("你还没有学习任何魂技")
                .notice("觉醒武魂后会获得基础魂技");
        }
        for entry in page.entries {
            document = document
                .line(format!(
                    "#{} · {} · Lv.{} · {} · {}魂力 · 冷却 {} 回合",
                    entry.skill.ring_index,
                    entry.skill.name,
                    entry.level,
                    if entry.equipped {
                        "已装备"
                    } else {
                        "未装备"
                    },
                    entry.skill.soul_power_cost,
                    entry.skill.cooldown_rounds
                ))
                .line(format!(
                    "熟练度：{}",
                    skill_proficiency_label(entry.level, entry.proficiency)
                ))
                .line(entry.skill.description.clone())
                .command(format!("技能详情 {}", entry.skill.name));
        }
        document.command("状态")
    }

    /// 按北京时间每日 04:00 划分游戏日，并由 Store 在单事务内发放奖励。
    pub fn open_wuhun(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        self.toggle_wuhun(req, true)
    }

    pub fn close_wuhun(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        self.toggle_wuhun(req, false)
    }

    fn toggle_wuhun(&self, req: &CommandRequest, enabled: bool) -> Result<GameDocument, String> {
        if !req.args.as_str().trim().is_empty() {
            return Err(format!("用法：{}武魂", if enabled { "开" } else { "关" }));
        }
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let details = operation_details(req, identity.protocol);
        let operation = successful_operation(req, &details);
        let receipt = self
            .store
            .set_wuhun_enabled_with_operation(&key, enabled, &operation)?;
        Ok(self.wuhun_toggle_document(receipt, enabled))
    }

    fn wuhun_toggle_document(&self, receipt: WuhunToggleReceipt, enabled: bool) -> GameDocument {
        let mut document = GameDocument::new(if enabled {
            "武魂开启"
        } else {
            "武魂关闭"
        })
        .field("武魂", receipt.state.name.clone())
        .field(
            "状态",
            if receipt.state.enabled {
                "开启"
            } else {
                "关闭"
            },
        )
        .field(
            "稳定度",
            format!(
                "{}/{}",
                receipt.state.stability, receipt.state.max_stability
            ),
        )
        .notice(if receipt.replayed {
            "检测到相同消息的重复请求，已返回原武魂状态，未重复执行"
        } else if enabled {
            "武魂已进入战斗形态；稳定度随生命变化，低于 30 时受击可能自动脱落"
        } else {
            "武魂已收回；关闭状态下不能发起魂兽挑战"
        });
        document = document.illustration_if(self.wuhun_illustration(&receipt.state.name));
        if receipt.state.enabled {
            document.command("关武魂").command("状态")
        } else {
            document.command("开武魂").command("状态")
        }
    }

    pub fn daily_checkin(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        if !req.args.as_str().trim().is_empty() {
            return Err("用法：签到".to_string());
        }
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let input = DailyCheckinInput {
            game_day: game_day_from_timestamp(current_unix_timestamp()?)?,
            currency_code: CHECKIN_CURRENCY_CODE,
            currency_reward_override: None,
        };
        let details = operation_details(req, identity.protocol);
        let operation = successful_operation(req, &details);
        let result = self.store.daily_checkin(&key, &input, &operation)?;
        let (receipt, already_claimed) = match result {
            DailyCheckinResult::Claimed(receipt) => (receipt, false),
            DailyCheckinResult::AlreadyClaimed(receipt) => (receipt, true),
        };
        if receipt.currency_code != CHECKIN_CURRENCY_CODE
            || checkin_exp_reward(receipt.cycle_day)? != receipt.exp_reward
        {
            return Err("签到记录的奖励规则与当前版本不一致".to_string());
        }
        let mut document = GameDocument::new("每日签到")
            .field(
                "结果",
                if already_claimed {
                    "今日已签到"
                } else {
                    "签到成功"
                },
            )
            .field("累计签到", format!("{} 天", receipt.total_claims))
            .field("连续签到", format!("{} 天", receipt.streak_days))
            .field("本轮", format!("第 {}/7 天", receipt.cycle_day))
            .field("当前境界", receipt.title_after.clone())
            .field("当前经验", receipt.exp_after.to_string())
            .field(
                format!("当前{CHECKIN_CURRENCY_NAME}"),
                receipt.currency_balance_after.to_string(),
            )
            .command("状态");
        document = if already_claimed {
            document
                .field("当日经验奖励", format!("{}（已领取）", receipt.exp_reward))
                .field(
                    format!("当日{CHECKIN_CURRENCY_NAME}奖励"),
                    format!("{}（已领取）", receipt.currency_reward),
                )
        } else {
            document
                .field("经验奖励", format!("+{}", receipt.exp_reward))
                .field(
                    format!("{CHECKIN_CURRENCY_NAME}奖励"),
                    format!("+{}", receipt.currency_reward),
                )
        };
        document = if already_claimed {
            document.notice("本游戏日已经签到过了，奖励不会重复发放")
        } else if receipt.levels_gained > 0 {
            document.notice(format!(
                "升级成功：{} → {}，继续积累经验解锁更高境界",
                receipt.level_before, receipt.title_after
            ))
        } else {
            document.notice("签到成功，下一个北京时间 04:00 刷新后可再次领取")
        };
        Ok(document)
    }

    pub fn wallet(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        if !req.args.as_str().trim().is_empty() {
            return Err("用法：钱包".to_string());
        }
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let balance = self
            .store
            .wallet_balance(&key, CHECKIN_CURRENCY_CODE)?
            .ok_or_else(|| "你还没有角色，请先使用“开始穿越 角色名 性别”".to_string())?;
        Ok(GameDocument::new("我的钱包")
            .field(CHECKIN_CURRENCY_NAME, balance.to_string())
            .command("签到")
            .notice("当前展示签到使用的金魂币余额"))
    }

    pub fn transfer_gold(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let identity = resolve_identity(req, &self.config.identity)?;
        let mention = resolve_target_mention(req, identity.protocol)?;
        let (recipient_subject_id, amount) = parse_transfer_args(
            req.args.as_str(),
            mention,
            self.config.messages.legacy_hyphen_arguments,
        )?;
        if recipient_subject_id == identity.subject_id {
            return Err("不能向自己转账".to_string());
        }
        let key = self.identity_key(&identity, &identity.subject_id);
        let details = operation_details(req, identity.protocol);
        let operation = successful_operation(req, &details);
        let receipt = self.store.transfer_gold_with_operation(
            &key,
            &recipient_subject_id,
            amount,
            &operation,
        )?;
        let notice = if receipt.replayed {
            "检测到相同消息的重复请求，已返回原转账回执，未再次扣款"
        } else {
            "双方钱包、操作日志与不可变转移账本已在同一事务完成"
        };
        Ok(GameDocument::new(if receipt.replayed {
            "转账回执"
        } else {
            "转账成功"
        })
        .field("收款用户", receipt.recipient_subject_id)
        .field(
            "转账金额",
            format!("{} {CHECKIN_CURRENCY_NAME}", receipt.amount),
        )
        .field("我的余额", receipt.sender_balance_after.to_string())
        .field("对方余额", receipt.recipient_balance_after.to_string())
        .field("账本编号", receipt.transfer_id.to_string())
        .notice(notice)
        .command("钱包"))
    }

    pub fn gift_item(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let identity = resolve_identity(req, &self.config.identity)?;
        let mention = resolve_target_mention(req, identity.protocol)?;
        let (recipient_subject_id, item_name, quantity) = parse_gift_args(
            req.args.as_str(),
            mention,
            self.config.messages.legacy_hyphen_arguments,
        )?;
        if recipient_subject_id == identity.subject_id {
            return Err("不能向自己发送物品".to_string());
        }
        let key = self.identity_key(&identity, &identity.subject_id);
        let details = operation_details(req, identity.protocol);
        let operation = successful_operation(req, &details);
        let receipt = self.store.gift_item_with_operation(
            &key,
            &recipient_subject_id,
            &item_name,
            quantity,
            &operation,
        )?;
        let notice = if receipt.replayed {
            "检测到相同消息的重复请求，已返回原赠送回执，未再次扣除物品"
        } else {
            "双方背包、操作日志与不可变转移账本已在同一事务完成"
        };
        Ok(GameDocument::new(if receipt.replayed {
            "物品赠送回执"
        } else {
            "物品赠送成功"
        })
        .field("接收用户", receipt.recipient_subject_id)
        .field(
            "赠送物品",
            format!("{} x{}", receipt.item.name, receipt.quantity),
        )
        .field("我的背包数量", receipt.sender_inventory_after.to_string())
        .field(
            "对方背包数量",
            receipt.recipient_inventory_after.to_string(),
        )
        .field("账本编号", receipt.transfer_id.to_string())
        .notice(notice)
        .command("背包"))
    }

    pub fn npcs(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        if !req.args.as_str().trim().is_empty() {
            return Err("用法：NPC".to_string());
        }
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let page = self.store.npcs_at_current_map(&key)?;
        let mut document = GameDocument::new(format!("当前 NPC · {}", page.map_name))
            .field("地图", &page.map_name)
            .field("数量", page.entries.len().to_string());
        if page.entries.is_empty() {
            document = document.line("当前地图没有可互动的 NPC");
        } else {
            for npc in page.entries {
                let kind = if npc.has_shop { "商人" } else { "NPC" };
                document = document.line(format!("{} · {}\n{}", npc.name, kind, npc.description));
                document = document.command(format!("对话 {}", npc.name));
            }
        }
        Ok(document.command("位置"))
    }

    pub fn talk(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let npc_name = parse_required_catalog_name(req.args.as_str(), "对话 <NPC>")?;
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let details = operation_details(req, identity.protocol);
        let operation = successful_operation(req, &details);
        let npc = self
            .store
            .talk_to_npc_with_operation(&key, npc_name, &operation)?;
        let mut document = GameDocument::new(format!("与 {} 对话", npc.name))
            .field("地图", &npc.map_name)
            .field("NPC", &npc.name)
            .field("身份", npc_kind_label(&npc.npc_kind))
            .line(&npc.dialogue)
            .line(&npc.description)
            .command("NPC")
            .command("位置");
        if npc.has_shop {
            document = document.command("商店").command("购买 <物品> [数量]");
        }
        Ok(document.notice("当前对话已绑定；移动到其他地图后需要重新对话"))
    }

    pub fn shop(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let (npc_name, page) = parse_shop_args(req.args.as_str())?;
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let shop = self.store.shop_items_page(&key, npc_name, page, 8)?;
        let mut document = GameDocument::new(format!("{}的商店", shop.npc.name))
            .field("地图", &shop.npc.map_name)
            .field("页码", format!("{} / {}", shop.page, shop.page_count))
            .field("商品总数", shop.total.to_string());
        if shop.entries.is_empty() {
            document = document.line("商店暂时没有商品");
        } else {
            for entry in &shop.entries {
                let stock = entry
                    .stock
                    .map_or_else(|| "不限量".to_string(), |value| format!("库存 {}", value));
                document = document.line(format!(
                    "{} · {} 金魂币 · 回收 {} · {}\n{}",
                    entry.item.name,
                    entry.price,
                    entry.item.sell_price,
                    stock,
                    item_effect_description(&entry.item)
                ));
            }
        }
        if page > 1 {
            document = document.command(format!("商店 {}", page - 1));
        }
        if page < shop.page_count {
            document = document.command(format!("商店 {}", page + 1));
        }
        Ok(document
            .command("购买 <物品> [数量]")
            .command("出售 <物品> [数量]")
            .command("背包"))
    }

    pub fn inventory(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let page = parse_single_page(req.args.as_str(), "背包")?;
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let inventory = self.store.inventory_page(&key, page, 8)?;
        let balance = self
            .store
            .wallet_balance(&key, CHECKIN_CURRENCY_CODE)?
            .unwrap_or(0);
        let mut document = GameDocument::new("随身物品")
            .field(
                "页码",
                format!("{} / {}", inventory.page, inventory.page_count),
            )
            .field("物品种类", inventory.total.to_string())
            .field(CHECKIN_CURRENCY_NAME, balance.to_string());
        if inventory.entries.is_empty() {
            document = document.line("背包为空");
        } else {
            for entry in &inventory.entries {
                document = document.line(format!(
                    "{} x{} · {}\n{}",
                    entry.item.name,
                    entry.quantity,
                    item_quality_label(entry.item.quality),
                    item_effect_description(&entry.item)
                ));
            }
        }
        if page > 1 {
            document = document.command(format!("背包 {}", page - 1));
        }
        if page < inventory.page_count {
            document = document.command(format!("背包 {}", page + 1));
        }
        Ok(document
            .command("使用 <物品>")
            .command("商店")
            .command("钱包"))
    }

    pub fn ground_drops(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let page = parse_single_page(req.args.as_str(), "掉落")?;
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let drops = self.store.ground_drops_page(&key, page, 8)?;
        let mut document = GameDocument::new(format!("当前掉落 · {}", drops.map_name))
            .field("页码", format!("{} / {}", drops.page, drops.page_count))
            .field("可拾取数量", drops.total.to_string());
        if drops.entries.is_empty() {
            document = document.line("当前地图没有可拾取的地面掉落");
        } else {
            for drop in &drops.entries {
                let owner = drop
                    .owner_subject_id
                    .as_deref()
                    .map_or_else(|| "公共掉落".to_string(), |owner| format!("归属 {owner}"));
                let expiry = drop.expires_at.map_or_else(
                    || "永久".to_string(),
                    |expires_at| format!("有效至时间戳 {expires_at}"),
                );
                document = document
                    .line(format!(
                        "#{} · {} x{} · {} · {}",
                        drop.id, drop.item.name, drop.quantity, owner, expiry
                    ))
                    .command(format!("拾取 {}", drop.id));
            }
        }
        if page > 1 {
            document = document.command(format!("掉落 {}", page - 1));
        }
        if page < drops.page_count {
            document = document.command(format!("掉落 {}", page + 1));
        }
        Ok(document.command("位置").command("背包"))
    }

    pub fn pick_up_ground_drop(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let drop_id = parse_ground_drop_id(req.args.as_str())?;
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let details = operation_details(req, identity.protocol);
        let operation = successful_operation(req, &details);
        let receipt = self
            .store
            .pick_up_ground_drop_with_operation(&key, drop_id, &operation)?;
        let title = if receipt.replayed {
            "拾取回执"
        } else {
            "拾取成功"
        };
        let notice = if receipt.replayed {
            "检测到相同消息的重复请求，已返回原拾取回执，未再次增加物品"
        } else {
            "背包、操作日志和不可变拾取账本已在同一事务完成"
        };
        Ok(GameDocument::new(title)
            .field("掉落编号", receipt.drop_id.to_string())
            .field(
                "拾取物品",
                format!("{} x{}", receipt.item.name, receipt.quantity),
            )
            .field("背包数量", receipt.inventory_after.to_string())
            .field("拾取账本编号", receipt.claim_id.to_string())
            .notice(notice)
            .command("掉落")
            .command("背包"))
    }

    pub fn quests(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let page = parse_single_page(req.args.as_str(), "任务")?;
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let quests = self.store.quests_page(&key, page, 8)?;
        let mut document = GameDocument::new(format!("任务列表 · {}", quests.map_name))
            .field("页码", format!("{} / {}", quests.page, quests.page_count))
            .field("任务总数", quests.total.to_string());
        if quests.entries.is_empty() {
            document = document.line("当前地图暂无可接取任务");
        } else {
            for entry in &quests.entries {
                document = self.append_quest_entry(document, entry);
            }
        }
        if page > 1 {
            document = document.command(format!("任务 {}", page - 1));
        }
        if page < quests.page_count {
            document = document.command(format!("任务 {}", page + 1));
        }
        Ok(document.command("任务进度").command("接取任务 <任务>"))
    }

    pub fn accept_quest(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let quest_name = parse_required_catalog_name(req.args.as_str(), "接取任务 <任务>")?;
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let details = operation_details(req, identity.protocol);
        let operation = successful_operation(req, &details);
        let receipt = self
            .store
            .accept_quest_with_operation(&key, quest_name, &operation)?;
        Ok(self.quest_action_document(receipt))
    }

    pub fn quest_progress(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let requested = parse_optional_quest_name(req.args.as_str())?;
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let entries = self.store.active_quests(&key, requested)?;
        let mut document =
            GameDocument::new("进行中任务").field("任务数量", entries.len().to_string());
        if entries.is_empty() {
            document = document.line("当前没有进行中的任务");
        } else {
            for entry in &entries {
                document = self.append_quest_entry(document, entry);
            }
        }
        Ok(document.command("任务").command("提交任务 <任务>"))
    }

    pub fn submit_quest(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let quest_name = parse_required_catalog_name(req.args.as_str(), "提交任务 <任务>")?;
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let details = operation_details(req, identity.protocol);
        let operation = successful_operation(req, &details);
        let receipt = self
            .store
            .submit_quest_with_operation(&key, quest_name, &operation)?;
        Ok(self.quest_action_document(receipt))
    }

    pub fn abandon_quest(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let quest_name = parse_required_catalog_name(req.args.as_str(), "放弃任务 <任务>")?;
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let details = operation_details(req, identity.protocol);
        let operation = successful_operation(req, &details);
        let receipt = self
            .store
            .abandon_quest_with_operation(&key, quest_name, &operation)?;
        Ok(self.quest_action_document(receipt))
    }

    pub fn soul_beasts(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let page = parse_single_page(req.args.as_str(), "魂兽")?;
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let beasts = self.store.soul_beasts_page(&key, page, 8)?;
        Ok(self.soul_beasts_document(beasts))
    }

    pub fn challenge(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let beast_name = parse_required_catalog_name(req.args.as_str(), "挑战 <魂兽>")?;
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let details = operation_details(req, identity.protocol);
        let operation = successful_operation(req, &details);
        let receipt = self
            .store
            .challenge_soul_beast_with_operation(&key, beast_name, &operation)?;
        Ok(self.battle_action_document(receipt))
    }

    pub fn attack(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        if !req.args.as_str().trim().is_empty() {
            return Err("用法：攻击".to_string());
        }
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let details = operation_details(req, identity.protocol);
        let operation = successful_operation(req, &details);
        let receipt = self.store.attack_battle_with_operation(&key, &operation)?;
        Ok(self.battle_action_document(receipt))
    }

    pub fn use_skill(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let skill_name = parse_required_catalog_name(req.args.as_str(), "释放技能 <魂技>")?;
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let details = operation_details(req, identity.protocol);
        let operation = successful_operation(req, &details);
        let receipt = self
            .store
            .use_skill_battle_with_operation(&key, skill_name, &operation)?;
        Ok(self.battle_action_document(receipt))
    }

    pub fn flee(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        if !req.args.as_str().trim().is_empty() {
            return Err("用法：逃跑".to_string());
        }
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let details = operation_details(req, identity.protocol);
        let operation = successful_operation(req, &details);
        let receipt = self.store.flee_battle_with_operation(&key, &operation)?;
        Ok(self.battle_action_document(receipt))
    }

    pub fn battle_status(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        if !req.args.as_str().trim().is_empty() {
            return Err("用法：战斗状态".to_string());
        }
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let battle = self
            .store
            .active_battle(&key)?
            .ok_or_else(|| "你当前不在战斗中，请先使用“挑战 <魂兽>”".to_string())?;
        Ok(self.battle_snapshot_document(battle, "当前战斗"))
    }

    pub fn battle_logs(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let limit = parse_optional_log_limit(req.args.as_str())?;
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let log = self
            .store
            .battle_log(&key, limit)?
            .ok_or_else(|| "还没有战斗记录，请先使用“挑战 <魂兽>”".to_string())?;
        Ok(self.battle_log_document(log))
    }

    fn soul_beasts_document(&self, beasts: SoulBeastPage) -> GameDocument {
        let mut document = GameDocument::new(format!("可挑战魂兽 · {}", beasts.map_name))
            .field("页码", format!("{} / {}", beasts.page, beasts.page_count))
            .field("魂兽数量", beasts.total.to_string());
        if beasts.entries.is_empty() {
            document = document.line("当前地图没有达到等级要求的魂兽");
        } else {
            for beast in &beasts.entries {
                document = document
                    .line(format!(
                        "#{} · {} · {}年 · Lv.{}",
                        beast.id, beast.name, beast.age, beast.level_required
                    ))
                    .line(format!(
                        "生命 {} · 攻击 {} · 防御 {} · 经验 {} · 掉落 {} x{}",
                        beast.max_hp,
                        beast.attack,
                        beast.defense,
                        beast.exp_reward,
                        beast.drop_item.name,
                        beast.drop_quantity
                    ))
                    .line(beast.description.clone())
                    .command(format!("挑战 {}", beast.name));
            }
        }
        if beasts.page > 1 {
            document = document.command(format!("魂兽 {}", beasts.page - 1));
        }
        if beasts.page < beasts.page_count {
            document = document.command(format!("魂兽 {}", beasts.page + 1));
        }
        document.command("位置").command("战斗状态")
    }

    fn battle_action_document(&self, receipt: BattleActionReceipt) -> GameDocument {
        let event = &receipt.event;
        let beast = &receipt.battle.beast;
        let title = match event.event_kind.as_str() {
            "challenge" => "战斗开始",
            "attack" if event.status_after == "won" => "战斗胜利",
            "attack" if event.status_after == "defeated" => "战斗失败",
            "attack" => "攻击结算",
            "flee" if event.flee_success == Some(true) => "逃跑成功",
            "flee" if event.status_after == "defeated" => "逃跑失败",
            "flee" => "逃跑失败",
            _ => "战斗回执",
        };
        let mut document = GameDocument::new(if receipt.replayed {
            format!("{title}回执")
        } else {
            title.to_string()
        })
        .field("对手", format!("{} · {}年", beast.name, beast.age))
        .field("回合", event.sequence.to_string())
        .field(
            "玩家生命",
            format!(
                "{} → {}/{}",
                event.player_hp_before, event.player_hp_after, receipt.battle.player_max_hp
            ),
        )
        .field(
            "魂兽生命",
            format!(
                "{} → {}/{}",
                event.beast_hp_before, event.beast_hp_after, receipt.battle.beast_max_hp
            ),
        )
        .field(
            "武魂战斗修正",
            format!(
                "攻击 {}% · 防御 {}%",
                receipt.battle.wuhun_attack_percent, receipt.battle.wuhun_defense_percent
            ),
        );
        if let Some(skill) = &receipt.skill {
            document = document
                .field("魂技", skill.skill.name.clone())
                .field(
                    "魂力",
                    format!("{} → {}", skill.soul_power_before, skill.soul_power_after),
                )
                .field("魂技冷却", format!("{} 回合", skill.skill.cooldown_rounds))
                .field(
                    "等级伤害倍率",
                    skill_damage_modifier_label(&skill.damage_modifier),
                );
            if let Some(progress) = &skill.progress {
                document = document.field(
                    "魂技熟练度",
                    format!(
                        "{} → {}（+{}）",
                        progress.proficiency_before,
                        progress.proficiency_after,
                        progress.proficiency_gain
                    ),
                );
                if progress.level_after > progress.level_before {
                    document = document.field(
                        "魂技升级",
                        format!("Lv.{} → Lv.{}", progress.level_before, progress.level_after),
                    );
                }
            }
            for effect in &skill.effects {
                document = document.field("附加效果", battle_skill_effect_label(effect));
            }
        }
        if event.event_kind == "challenge" {
            document = document
                .line(beast.description.clone())
                .line("魂兽会在你的每次行动后反击");
        } else if event.event_kind == "attack" {
            document = document.line(if let Some(skill) = &receipt.skill {
                format!(
                    "你释放{}，造成 {} 点{}伤害",
                    skill.skill.name,
                    event.player_damage,
                    if event.player_critical { "暴击" } else { "" }
                )
            } else {
                format!(
                    "你造成 {} 点{}伤害",
                    event.player_damage,
                    if event.player_critical { "暴击" } else { "" }
                )
            });
            if event.beast_damage > 0 {
                document = document.line(format!(
                    "魂兽反击造成 {} 点{}伤害",
                    event.beast_damage,
                    if event.beast_critical { "暴击" } else { "" }
                ));
            }
        } else if event.flee_success == Some(true) {
            document = document.line("你脱离了战斗，魂兽没有追击");
        } else {
            document = document.line(format!(
                "逃跑失败，魂兽反击造成 {} 点{}伤害",
                event.beast_damage,
                if event.beast_critical { "暴击" } else { "" }
            ));
        }
        if let Some(effect) = &receipt.wuhun_effect {
            document = document.field(
                "武魂稳定度",
                format!("{} → {}", effect.stability_before, effect.stability_after),
            );
        }
        for effect in &receipt.expired_effects {
            document = document.line(format!(
                "效果结束：{}（{}）",
                effect.skill_name,
                battle_skill_effect_short_label(effect)
            ));
        }
        for effect in &receipt.battle.active_effects {
            document = document.field(
                "生效中",
                active_battle_skill_effect_label(effect, receipt.battle.action_count),
            );
        }
        if event.status_after == "won" {
            document = document
                .field("获得经验", event.experience_awarded.to_string())
                .line("魂兽倒下，地面出现了新的掉落");
            if let Some(drop) = &receipt.soul_ring_drop {
                document = document
                    .field("待吸收魂环", drop.ring.name.clone())
                    .line(format!(
                        "可使用“吸收魂环 {}”吸收并解锁魂技 {}",
                        drop.ring.soul_beast_name, drop.ring.skill.name
                    ));
            }
            document = document.notice(if receipt.replayed {
                "检测到相同消息的重复请求，已返回原战斗回执，未重复发放经验或掉落"
            } else {
                "战斗状态、生命、经验、掉落、操作日志和战斗事件已在同一事务完成"
            });
        } else if event.status_after == "defeated" {
            document = document
                .line("你被魂兽击败，系统将你救回到濒死状态")
                .notice("死亡与复活系统尚未开放，本阶段不会删除角色或扣除转生次数");
        } else if receipt.replayed {
            document = document.notice("检测到相同消息的重复请求，已返回原战斗回执，未重复执行");
        } else {
            document = document.notice("本回合已在同一事务内结算，战斗快照可在热重载后继续");
        }
        if receipt
            .wuhun_effect
            .as_ref()
            .is_some_and(|effect| effect.auto_dropped)
        {
            document = document.notice(if receipt.replayed {
                "检测到相同消息的重复请求，已返回原回执；该回合武魂因稳定度过低自动脱落"
            } else if event.status_after == "defeated" {
                "武魂因稳定度过低自动脱落；你已被救回濒死状态，死亡与复活系统尚未开放"
            } else {
                "武魂稳定度过低，武魂已自动脱落；结束战斗后可重新开启"
            });
        }
        document =
            document.illustration_if(self.asset_illustration("soul_beast", &beast.name, "battle"));
        match event.status_after.as_str() {
            "active" => document
                .command("攻击")
                .command("释放技能 <魂技>")
                .command("逃跑")
                .command("战斗状态"),
            "won" => {
                let mut document = document.command("掉落").command("魂兽").command("魂环");
                if let Some(drop) = &receipt.soul_ring_drop {
                    document = document.command(format!("吸收魂环 {}", drop.ring.soul_beast_name));
                }
                document
            }
            _ => document.command("魂兽").command("状态"),
        }
    }

    fn battle_snapshot_document(&self, battle: BattleSnapshot, title: &str) -> GameDocument {
        let mut document = GameDocument::new(title)
            .field(
                "对手",
                format!("{} · {}年", battle.beast.name, battle.beast.age),
            )
            .field("状态", battle_status_label(&battle.status))
            .field("回合", battle.action_count.to_string())
            .field(
                "武魂战斗修正",
                format!(
                    "攻击 {}% · 防御 {}%",
                    battle.wuhun_attack_percent, battle.wuhun_defense_percent
                ),
            )
            .field(
                "玩家生命",
                format!("{}/{}", battle.player_hp, battle.player_max_hp),
            )
            .field(
                "魂兽生命",
                format!("{}/{}", battle.beast_hp, battle.beast_max_hp),
            );
        for effect in &battle.active_effects {
            document = document.field(
                "生效中",
                active_battle_skill_effect_label(effect, battle.action_count),
            );
        }
        document
            .illustration_if(self.asset_illustration("soul_beast", &battle.beast.name, "battle"))
            .command("攻击")
            .command("释放技能 <魂技>")
            .command("逃跑")
            .command("战斗日志")
    }

    fn battle_log_document(&self, log: BattleLog) -> GameDocument {
        let mut document = self
            .battle_snapshot_document(log.battle.clone(), "战斗日志")
            .line("事件记录：");
        for event in &log.events {
            document = document.line(format_battle_event(event));
        }
        document
    }

    fn append_quest_entry(
        &self,
        mut document: GameDocument,
        entry: &QuestListEntry,
    ) -> GameDocument {
        let status = match entry.status.as_deref() {
            Some("active") => "进行中",
            Some("completed") => "已完成",
            Some("abandoned") => "已放弃",
            _ => "可接取",
        };
        document = document
            .line(format!(
                "#{} · {} · {} · Lv.{}",
                entry.quest.id, entry.quest.name, status, entry.quest.level_required
            ))
            .line(entry.quest.description.clone());
        for requirement in &entry.progress {
            document = document.line(format!(
                "条件：{} {}/{}",
                requirement.description, requirement.current_amount, requirement.required_quantity
            ));
        }
        if !entry.rewards.is_empty() {
            let rewards = entry
                .rewards
                .iter()
                .map(|reward| match reward.reward_kind.as_str() {
                    "exp" => format!("经验 x{}", reward.amount),
                    "currency" => format!(
                        "{} x{}",
                        reward.currency_code.as_deref().unwrap_or("货币"),
                        reward.amount
                    ),
                    "item" => format!(
                        "{} x{}",
                        reward
                            .item
                            .as_ref()
                            .map(|item| item.name.as_str())
                            .unwrap_or("物品"),
                        reward.amount
                    ),
                    _ => format!("奖励 x{}", reward.amount),
                })
                .collect::<Vec<_>>()
                .join("、");
            document = document.line(format!("奖励：{rewards}"));
        }
        match entry.status.as_deref() {
            Some("active") => document.command(format!("任务进度 {}", entry.quest.name)),
            None => document.command(format!("接取任务 {}", entry.quest.name)),
            _ => document,
        }
    }

    fn quest_action_document(&self, receipt: QuestActionReceipt) -> GameDocument {
        let title = if receipt.replayed {
            format!("{}回执", receipt.action)
        } else {
            format!("{}成功", receipt.action)
        };
        let mut document = GameDocument::new(title)
            .field("任务", receipt.quest.name)
            .field("状态", receipt.action.clone());
        if !receipt.progress.is_empty() {
            document = document.line("任务条件：");
            for requirement in &receipt.progress {
                document = document.line(format!(
                    "{} {}/{}",
                    requirement.description,
                    requirement.current_amount,
                    requirement.required_quantity
                ));
            }
        }
        if !receipt.rewards.is_empty() && receipt.action == "提交任务" {
            document = document.line("获得奖励：");
            for reward in &receipt.rewards {
                let text = match reward.reward_kind.as_str() {
                    "exp" => format!("经验 x{}", reward.amount),
                    "currency" => format!(
                        "{} x{}",
                        reward.currency_code.as_deref().unwrap_or("货币"),
                        reward.amount
                    ),
                    "item" => format!(
                        "{} x{}",
                        reward
                            .item
                            .as_ref()
                            .map(|item| item.name.as_str())
                            .unwrap_or("物品"),
                        reward.amount
                    ),
                    _ => format!("奖励 x{}", reward.amount),
                };
                document = document.line(text);
            }
        }
        if let Some(experience) = receipt.experience {
            document = document
                .field(
                    "经验",
                    format!("{} → {}", experience.exp_before, experience.exp_after),
                )
                .field(
                    "等级",
                    format!("{} → {}", experience.level_before, experience.level_after),
                );
        }
        if let Some(balance) = receipt.currency_balance_after {
            document = document.field("金魂币余额", balance.to_string());
        }
        document
            .notice(if receipt.replayed {
                "检测到相同消息的重复请求，已返回原任务回执，未重复发放奖励"
            } else {
                "任务状态、奖励、背包、钱包和操作日志已在同一事务完成"
            })
            .command("任务进度")
            .command("任务")
    }

    pub fn buy(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let (item_name, quantity) = parse_item_quantity(
            req.args.as_str(),
            self.config.messages.legacy_hyphen_arguments,
            "购买 <物品> [数量]",
        )?;
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let details = operation_details(req, identity.protocol);
        let operation = successful_operation(req, &details);
        let receipt = self
            .store
            .buy_item_with_operation(&key, item_name, quantity, &operation)?;
        Ok(GameDocument::new("购买成功")
            .field("NPC", receipt.npc_name)
            .field(
                "物品",
                format!("{} x{}", receipt.item.name, receipt.quantity),
            )
            .field("花费", format!("{} 金魂币", receipt.total_price))
            .field("钱包余额", receipt.balance_after.to_string())
            .field("背包数量", receipt.inventory_after.to_string())
            .notice("购买、扣款和背包入账已在同一事务完成")
            .command("背包")
            .command("商店"))
    }

    pub fn sell(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let (item_name, quantity) = parse_item_quantity(
            req.args.as_str(),
            self.config.messages.legacy_hyphen_arguments,
            "出售 <物品> [数量]",
        )?;
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let details = operation_details(req, identity.protocol);
        let operation = successful_operation(req, &details);
        let receipt = self
            .store
            .sell_item_with_operation(&key, item_name, quantity, &operation)?;
        Ok(GameDocument::new("出售成功")
            .field("NPC", receipt.npc_name)
            .field(
                "物品",
                format!("{} x{}", receipt.item.name, receipt.quantity),
            )
            .field("收入", format!("{} 金魂币", receipt.total_price))
            .field("钱包余额", receipt.balance_after.to_string())
            .field("背包数量", receipt.inventory_after.to_string())
            .notice("出售、背包扣除和钱包入账已在同一事务完成")
            .command("背包")
            .command("商店"))
    }

    pub fn use_item(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let item_name = parse_required_catalog_name(req.args.as_str(), "使用 <物品>")?;
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let details = operation_details(req, identity.protocol);
        let operation = successful_operation(req, &details);
        let receipt = self
            .store
            .use_item_with_operation(&key, item_name, &operation)?;
        let mut document = GameDocument::new(format!("使用{}", receipt.item.name))
            .field("背包数量", receipt.inventory_after.to_string())
            .field(
                "生命",
                format!(
                    "{} → {}/{}",
                    receipt.hp_before, receipt.hp_after, receipt.max_hp
                ),
            )
            .field(
                "魂力",
                format!(
                    "{} → {}/{}",
                    receipt.soul_power_before, receipt.soul_power_after, receipt.max_soul_power
                ),
            );
        if let (Some(enabled), Some(before), Some(after)) = (
            receipt.wuhun_enabled,
            receipt.wuhun_stability_before,
            receipt.wuhun_stability_after,
        ) {
            document = document.field(
                "武魂稳定度",
                format!(
                    "{} → {}/{}",
                    before,
                    after,
                    receipt.wuhun_max_stability.unwrap_or(100)
                ),
            );
            if !enabled {
                document = document.line("武魂当前处于收回状态，稳定度会随生命恢复同步");
            }
        }
        if receipt.consumed {
            document = document.notice("物品已消耗");
        } else {
            document = document.notice("当前属性已满，物品未消耗");
        }
        Ok(document.command("背包").command("状态"))
    }

    pub fn status(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
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
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let player = self
            .store
            .player_status(&key)?
            .ok_or_else(|| "你还没有角色，请先使用“开始穿越 角色名 性别”".to_string())?;
        let map = self
            .store
            .current_map(&key)?
            .ok_or_else(|| "角色的地图存档不存在，请联系管理员修复地图绑定".to_string())?;
        let exits = self.store.map_exits(&key)?;
        Ok(self.world_document(&map, &exits, Some(&player)))
    }

    pub fn map_list(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let page = parse_map_page(req.args.as_str())?;
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let player = self
            .store
            .player_status(&key)?
            .ok_or_else(|| "你还没有角色，请先使用“开始穿越 角色名 性别”".to_string())?;
        let maps = self.store.list_maps_page(page, MAP_PAGE_SIZE)?;
        let mut document = GameDocument::new("地图列表")
            .field("页码", format!("{} / {}", maps.page, maps.page_count))
            .field("地图总数", maps.total.to_string());
        for map in &maps.entries {
            let mut tags = Vec::new();
            if map.safe {
                tags.push("安全区");
            } else if map.pvp_enabled {
                tags.push("可战斗");
            }
            if map.teleport_enabled {
                tags.push("传送阵");
            }
            if player.level < map.level_required {
                tags.push("等级不足");
            }
            let suffix = if tags.is_empty() {
                String::new()
            } else {
                format!(" [{}]", tags.join("、"))
            };
            document = document.line(format!(
                "{} · Lv.{}{}\n{}",
                map.name, map.level_required, suffix, map.description
            ));
        }
        if page > 1 {
            document = document.command(format!("地图列表 {}", page - 1));
        }
        if page < maps.page_count {
            document = document.command(format!("地图列表 {}", page + 1));
        }
        Ok(document
            .command("位置")
            .command("传送 <地图>")
            .notice("地图拓扑来自游戏数据表；图片资源只负责展示，不决定可通行方向"))
    }

    pub fn move_direction(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let direction = parse_direction_arg(req.args.as_str())?;
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let details = operation_details(req, identity.protocol);
        let operation = successful_operation(req, &details);
        let receipt = self
            .store
            .move_direction_with_operation(&key, direction, &operation)?;
        self.transition_document(&key, receipt, "移动成功")
    }

    pub fn teleport(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let target_name = parse_optional_map_name(req.args.as_str())?;
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let details = operation_details(req, identity.protocol);
        let operation = successful_operation(req, &details);
        let receipt = self
            .store
            .teleport_with_operation(&key, target_name, &operation)?;
        self.transition_document(&key, receipt, "传送成功")
    }

    fn transition_document(
        &self,
        key: &IdentityKey<'_>,
        receipt: MapTravelReceipt,
        title: &str,
    ) -> Result<GameDocument, String> {
        let exits = self.store.map_exits(key)?;
        let document = self
            .world_document(&receipt.to, &exits, None)
            .field("结果", title)
            .field("出发地", receipt.from.name)
            .field("到达地", receipt.to.name);
        Ok(document.notice(match receipt.travel_kind.as_str() {
            "teleport" => "传送阵已完成定位",
            _ => "已沿地图出口抵达目标区域",
        }))
    }

    fn world_document(
        &self,
        map: &MapRecord,
        exits: &[MapExit],
        player: Option<&PlayerStatus>,
    ) -> GameDocument {
        let mut document = GameDocument::new(format!("当前位置 · {}", map.name))
            .field("地图", &map.name)
            .field("简介", &map.description)
            .field("等级要求", format!("{} 级", map.level_required))
            .field(
                "区域",
                if map.safe {
                    "安全区"
                } else if map.pvp_enabled {
                    "野外区域（可战斗）"
                } else {
                    "普通区域"
                },
            )
            .field(
                "传送阵",
                if map.teleport_enabled {
                    "可用"
                } else {
                    "无"
                },
            );
        if let Some(player) = player {
            document = document
                .field("角色", &player.name)
                .field("生命", format!("{}/{}", player.hp, player.max_hp))
                .field(
                    "魂力",
                    format!("{}/{}", player.soul_power, player.max_soul_power),
                );
        }
        let walk_exits = exits
            .iter()
            .filter(|exit| exit.travel_kind == "walk")
            .map(|exit| {
                format!(
                    "{}：{}",
                    display_direction(exit.direction.as_deref().unwrap_or_default()),
                    exit.target.name
                )
            })
            .collect::<Vec<_>>();
        document = document.field(
            "方向",
            if walk_exits.is_empty() {
                "无可通行方向".to_string()
            } else {
                walk_exits.join("、")
            },
        );
        if exits.iter().any(|exit| exit.travel_kind == "teleport") {
            document = document.field("传送目标", "可使用“传送 地图名称”查看并前往");
        }
        document
            .illustration_if(self.asset_illustration("map", &map.name, "cover"))
            .command("状态")
            .command("NPC")
            .command("地图列表")
            .command("向 <上|下|左|右>")
            .command("传送 <地图>")
    }

    pub fn inspect_legacy(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let target_subject_id = parse_legacy_inspect_args(req.args.as_str())?;
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, target_subject_id);
        let (status, notice) = match self.store.inspect_legacy_identity(&key)? {
            LegacyIdentityState::Legacy => (
                "待认领",
                "确认目标用户和当前机器人账号无误后，使用“旧档认领 <用户ID> <当前account_id> 确认”",
            ),
            LegacyIdentityState::ClaimedToCurrent => {
                ("已认领到当前账号", "该旧档已完成绑定，无需重复操作")
            }
            LegacyIdentityState::ClaimedToOther => (
                "冲突",
                "该旧档已由其他机器人账号认领；系统不会合并、删除或重新绑定",
            ),
            LegacyIdentityState::Missing => (
                "未找到",
                "没有找到可认领的旧版身份行；系统不会自动创建或接管旧档",
            ),
        };
        Ok(GameDocument::new("旧档检查")
            .field("协议", identity.protocol.as_str())
            .field("当前账号", &identity.account_id)
            .field("目标用户", target_subject_id)
            .field("状态", status)
            .notice(notice))
    }

    pub fn claim_legacy(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let (target_subject_id, confirmed_account_id) = parse_legacy_claim_args(req.args.as_str())?;
        let identity = resolve_identity(req, &self.config.identity)?;
        if confirmed_account_id != identity.account_id {
            return Err("参数中的 account_id 与当前机器人账号不一致，已取消认领".to_string());
        }
        let key = self.identity_key(&identity, target_subject_id);
        let actor = LegacyClaimActor {
            account_id: &identity.account_id,
            subject_id: &identity.subject_id,
            message_id: req.message_id.as_str(),
            reason: "owner-explicit-legacy-claim",
        };
        let details = operation_details(req, identity.protocol);
        let operation = successful_operation(req, &details);
        let result =
            self.store
                .claim_legacy_identity_with_operation(&key, &actor, Some(&operation))?;
        let (status, notice) = match result {
            LegacyClaimResult::Claimed { .. } => (
                "认领成功",
                "旧身份仅补写了当前 account_id；角色、武魂、主键和外键均保持不变",
            ),
            LegacyClaimResult::AlreadyClaimed { .. } => {
                ("已认领", "该旧档已经绑定到当前账号，本次没有重复写入")
            }
            LegacyClaimResult::NotFound => {
                ("未找到", "没有找到可认领的旧版身份行，本次没有修改数据库")
            }
            LegacyClaimResult::Conflict => (
                "冲突",
                "检测到已知身份或其他账号的认领记录，本次没有合并、删除或重新绑定",
            ),
        };
        Ok(GameDocument::new("旧档认领")
            .field("协议", identity.protocol.as_str())
            .field("当前账号", &identity.account_id)
            .field("目标用户", target_subject_id)
            .field("结果", status)
            .notice(notice))
    }

    pub fn grant_context(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let (context_kind, context_id, label) = parse_grant_context_args(req.args.as_str())?;
        let identity = resolve_identity(req, &self.config.identity)?;
        validate_context_kind_for_protocol(identity.protocol, &context_kind)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let details = context_operation_details(req, identity.protocol, &context_kind, &context_id);
        let operation = successful_operation(req, &details);
        let result = self.store.grant_authorized_context(
            &key,
            &context_kind,
            &context_id,
            &label,
            &operation,
        )?;
        let (status, duplicate) = match result {
            AuthorizedContextChange::Granted { .. } => ("授权成功", false),
            AuthorizedContextChange::AlreadyGranted { .. } => ("已授权", true),
            AuthorizedContextChange::Revoked { .. } | AuthorizedContextChange::AlreadyRevoked => {
                return Err("授权上下文返回了不一致的状态".to_string());
            }
        };
        let mut document = GameDocument::new("授权上下文")
            .field("类型", &context_kind)
            .field("上下文 ID", &context_id)
            .field("结果", status);
        if duplicate {
            document =
                document.notice("该上下文已存在，原标签未修改；请使用“查看授权”确认当前标签");
        } else {
            document = document
                .field(
                    "标签",
                    if label.is_empty() {
                        "未设置"
                    } else {
                        &label
                    },
                )
                .notice("allowlist 模式下，该上下文现在可以执行游戏命令");
        }
        Ok(document)
    }

    pub fn revoke_context(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let (context_kind, context_id) = parse_revoke_context_args(req.args.as_str())?;
        let identity = resolve_identity(req, &self.config.identity)?;
        validate_context_kind_for_protocol(identity.protocol, &context_kind)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let details = context_operation_details(req, identity.protocol, &context_kind, &context_id);
        let operation = successful_operation(req, &details);
        let result =
            self.store
                .revoke_authorized_context(&key, &context_kind, &context_id, &operation)?;
        let status = match result {
            AuthorizedContextChange::Revoked { .. } => "已撤销",
            AuthorizedContextChange::AlreadyRevoked => "此前未授权",
            AuthorizedContextChange::Granted { .. }
            | AuthorizedContextChange::AlreadyGranted { .. } => {
                return Err("撤销授权上下文返回了不一致的状态".to_string());
            }
        };
        Ok(GameDocument::new("取消授权")
            .field("类型", &context_kind)
            .field("上下文 ID", &context_id)
            .field("结果", status)
            .notice("撤销不会删除操作日志；allowlist 模式会立即拒绝该上下文的游戏命令"))
    }

    pub fn list_contexts(&self, req: &CommandRequest) -> Result<GameDocument, String> {
        let after_id = parse_context_cursor(req.args.as_str())?;
        let identity = resolve_identity(req, &self.config.identity)?;
        let key = self.identity_key(&identity, &identity.subject_id);
        let page = self.store.list_authorized_contexts(&key, after_id, 10)?;
        let mut document = GameDocument::new("授权上下文列表")
            .field(
                "策略",
                match self.config.authorization.mode {
                    AuthorizationMode::AllowAll => "allow_all",
                    AuthorizationMode::Allowlist => "allowlist",
                },
            )
            .field("数量", page.entries.len().to_string());
        if page.entries.is_empty() {
            document = document.line("当前账号尚未登记授权上下文。")
        } else {
            for entry in page.entries {
                let label = if entry.label.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", entry.label)
                };
                document = document.line(format!(
                    "#{} · {} · {}{}",
                    entry.id, entry.context_kind, entry.context_id, label
                ));
            }
        }
        if let Some(cursor) = page.next_after_id {
            document = document.command(format!("查看授权 {cursor}"));
        }
        Ok(document.notice("列表按当前协议、Bot account_id 和 namespace 隔离"))
    }

    fn identity_key<'a>(
        &'a self,
        identity: &'a ResolvedIdentity,
        subject_id: &'a str,
    ) -> IdentityKey<'a> {
        IdentityKey {
            protocol: identity.protocol,
            account_id: &identity.account_id,
            namespace: &self.config.identity.namespace,
            // 同一 OneBot 用户在群聊和私聊中必须命中同一份角色存档。
            subject_kind: "user",
            subject_id,
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
        let progress = experience_progress(player.level, player.exp).ok();
        let realm = progress
            .map(|progress| format!("{}级{}", progress.level, progress.title))
            .unwrap_or_else(|| format!("{}级未知", player.level));
        let progress_text = match progress {
            Some(progress) => match progress.exp_for_next {
                Some(required) => format!("{} / {}", progress.exp_in_level, required),
                None => format!("{}（满级）", progress.total_exp),
            },
            None => "不可用".to_string(),
        };
        let mut document = GameDocument::new("角色状态")
            .field("角色", player.name)
            .field("性别", player.gender)
            .field("境界", realm)
            .field("经验", player.exp.to_string())
            .field("升级进度", progress_text)
            .field("生命", format!("{}/{}", player.hp, player.max_hp))
            .field(
                "魂力",
                format!("{}/{}", player.soul_power, player.max_soul_power),
            )
            .field("武魂", wuhun)
            .field("位置", player.map_name)
            .field("转生", format!("第 {} 世", player.life_count))
            .field("状态", player.state);
        if let Some(enabled) = player.wuhun_enabled {
            document = document.field("武魂状态", if enabled { "开启" } else { "关闭" });
            if let (Some(stability), Some(max_stability)) =
                (player.wuhun_stability, player.wuhun_max_stability)
            {
                document = document.field("武魂稳定度", format!("{stability}/{max_stability}"));
            }
        }
        document.illustration_if(illustration)
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

fn operation_details(req: &CommandRequest, protocol: crate::message::Protocol) -> String {
    let context = resolve_conversation_context(req, protocol)
        .map(|context| context.audit_kind())
        .unwrap_or("system");
    serde_json::json!({
        "context": context,
        "has_args": !req.args.as_str().trim().is_empty(),
    })
    .to_string()
}

fn context_operation_details(
    req: &CommandRequest,
    protocol: crate::message::Protocol,
    context_kind: &str,
    context_id: &str,
) -> String {
    let context = resolve_conversation_context(req, protocol)
        .map(|context| context.audit_kind())
        .unwrap_or("system");
    serde_json::json!({
        "context": context,
        "has_args": !req.args.as_str().trim().is_empty(),
        "target_kind": context_kind,
        "target_id": context_id,
    })
    .to_string()
}

fn successful_operation<'a>(
    req: &'a CommandRequest,
    details_json: &'a str,
) -> OperationLogInput<'a> {
    OperationLogInput {
        command: req.command_name.as_str(),
        outcome: "ok",
        source_message_id: req.message_id.as_str(),
        details_json,
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

fn parse_grant_context_args(args: &str) -> Result<(String, String, String), String> {
    let parts = args.split_whitespace().collect::<Vec<_>>();
    match parts.as_slice() {
        [context_id] => Ok((
            "group".to_string(),
            (*context_id).to_string(),
            String::new(),
        )),
        [context_kind, context_id, rest @ ..] if !rest.is_empty() => Ok((
            (*context_kind).to_string(),
            (*context_id).to_string(),
            rest.join(" "),
        )),
        [context_kind, context_id] => Ok((
            (*context_kind).to_string(),
            (*context_id).to_string(),
            String::new(),
        )),
        _ => {
            Err("用法：授权上下文 <group|channel> <上下文ID> [标签]；旧格式可只填群号".to_string())
        }
    }
}

fn parse_revoke_context_args(args: &str) -> Result<(String, String), String> {
    let parts = args.split_whitespace().collect::<Vec<_>>();
    match parts.as_slice() {
        [context_kind, context_id, "确认"] => {
            Ok(((*context_kind).to_string(), (*context_id).to_string()))
        }
        _ => Err("用法：取消授权 <group|channel> <上下文ID> 确认".to_string()),
    }
}

fn parse_context_cursor(args: &str) -> Result<Option<i64>, String> {
    let args = args.trim();
    if args.is_empty() {
        return Ok(None);
    }
    let cursor = args
        .parse::<i64>()
        .map_err(|_| "用法：查看授权 [下一页游标]".to_string())?;
    if cursor < 0 {
        return Err("授权列表游标不能为负数".to_string());
    }
    Ok(Some(cursor))
}

fn parse_map_page(args: &str) -> Result<usize, String> {
    let args = args.trim();
    if args.is_empty() {
        return Ok(1);
    }
    let mut parts = args.split_whitespace();
    let page = parts
        .next()
        .ok_or_else(|| "用法：地图列表 [页码]".to_string())?
        .parse::<usize>()
        .map_err(|_| "用法：地图列表 [页码]".to_string())?;
    if parts.next().is_some() || page == 0 || page > 100 {
        return Err("地图页码必须在 1 到 100 之间".to_string());
    }
    Ok(page)
}

fn parse_direction_arg(args: &str) -> Result<&str, String> {
    let mut parts = args.split_whitespace();
    let direction = parts.next().unwrap_or_default();
    if direction.is_empty() || parts.next().is_some() {
        return Err("用法：向 <上|下|左|右>".to_string());
    }
    match direction {
        "上" | "下" | "左" | "右" | "北" | "南" | "西" | "东" | "north" | "south" | "west"
        | "east" => Ok(direction),
        _ => Err("方向只能是上、下、左或右".to_string()),
    }
}

fn parse_optional_map_name(args: &str) -> Result<Option<&str>, String> {
    let args = args.trim();
    if args.is_empty() {
        return Ok(None);
    }
    if args.split_whitespace().count() != 1
        || args.chars().count() > 128
        || args.chars().any(char::is_control)
    {
        return Err("用法：传送 [地图名称]".to_string());
    }
    Ok(Some(args))
}

fn parse_optional_quest_name(args: &str) -> Result<Option<&str>, String> {
    let args = args.trim();
    if args.is_empty() {
        return Ok(None);
    }
    if args.chars().count() > 128 || args.chars().any(char::is_control) {
        return Err("任务名称不能超过 128 个字符且不能包含控制字符".to_string());
    }
    Ok(Some(args))
}

fn parse_required_catalog_name<'a>(args: &'a str, usage: &str) -> Result<&'a str, String> {
    let name = args.trim();
    if name.is_empty() {
        return Err(format!("用法：{usage}"));
    }
    if name.chars().count() > 128 || name.chars().any(char::is_control) {
        return Err("名称不能超过 128 个字符且不能包含控制字符".to_string());
    }
    Ok(name)
}

fn parse_single_page(args: &str, label: &str) -> Result<usize, String> {
    let args = args.trim();
    if args.is_empty() {
        return Ok(1);
    }
    let mut parts = args.split_whitespace();
    let page = parts
        .next()
        .ok_or_else(|| format!("用法：{label} [页码]"))?
        .parse::<usize>()
        .map_err(|_| format!("用法：{label} [页码]"))?;
    if parts.next().is_some() || page == 0 || page > 100 {
        return Err(format!("{label}页码必须在 1 到 100 之间"));
    }
    Ok(page)
}

fn parse_ground_drop_id(args: &str) -> Result<i64, String> {
    let mut parts = args.split_whitespace();
    let drop_id = parts
        .next()
        .ok_or_else(|| "用法：拾取 <掉落ID>".to_string())?
        .parse::<i64>()
        .map_err(|_| "掉落编号必须是正整数".to_string())?;
    if parts.next().is_some() || drop_id <= 0 {
        return Err("用法：拾取 <掉落ID>".to_string());
    }
    Ok(drop_id)
}

fn parse_shop_args(args: &str) -> Result<(Option<&str>, usize), String> {
    let args = args.trim();
    if args.is_empty() {
        return Ok((None, 1));
    }
    if let Ok(page) = args.parse::<usize>() {
        if (1..=100).contains(&page) {
            return Ok((None, page));
        }
        return Err("商店页码必须在 1 到 100 之间".to_string());
    }
    let mut parts = args.split_whitespace();
    let npc = parts.next().unwrap_or_default();
    let rest = parts.next();
    if rest.is_none() {
        return Ok((Some(args), 1));
    }
    if parts.next().is_some() {
        return Err("用法：商店 [页码]；如需指定 NPC，请先使用“对话 NPC”".to_string());
    }
    let page = rest
        .and_then(|value| value.parse::<usize>().ok())
        .ok_or_else(|| "用法：商店 [页码]；如需指定 NPC，请先使用“对话 NPC”".to_string())?;
    if !(1..=100).contains(&page) {
        return Err("商店页码必须在 1 到 100 之间".to_string());
    }
    Ok((Some(npc), page))
}

fn parse_item_quantity<'a>(
    args: &'a str,
    legacy_hyphen: bool,
    usage: &str,
) -> Result<(&'a str, i64), String> {
    let args = args.trim();
    if args.is_empty() {
        return Err(format!("用法：{usage}"));
    }

    // 新格式允许“物品名 数量”，并保留旧版“物品名-数量”。物品名本身
    // 可以包含空格，因此只把末尾的纯数字片段当作数量。
    let mut end = args.len();
    while end > 0 && args.as_bytes()[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    let number_end = end;
    let mut start = number_end;
    while start > 0 && !args.as_bytes()[start - 1].is_ascii_whitespace() {
        start -= 1;
    }
    if start < number_end
        && let Ok(quantity) = args[start..number_end].parse::<i64>()
    {
        let item_name = args[..start].trim_end();
        if !item_name.is_empty() {
            if !(1..=9999).contains(&quantity) {
                return Err("数量必须在 1 到 9999 之间".to_string());
            }
            return Ok((item_name, quantity));
        }
    }

    if legacy_hyphen
        && let Some((item_name, quantity_text)) = args.rsplit_once('-')
        && let Ok(quantity) = quantity_text.trim().parse::<i64>()
        && !item_name.trim().is_empty()
    {
        if !(1..=9999).contains(&quantity) {
            return Err("数量必须在 1 到 9999 之间".to_string());
        }
        return Ok((item_name.trim(), quantity));
    }
    Ok((args, 1))
}

fn parse_transfer_args(
    args: &str,
    mention: Option<String>,
    legacy_hyphen: bool,
) -> Result<(String, i64), String> {
    let args = args.trim();
    if let Some(target) = mention {
        let mut parts = args.split_whitespace();
        let amount = parts
            .next()
            .ok_or_else(|| format!("用法：{TRANSFER_USAGE}"))?;
        if parts.next().is_some() {
            return Err("已使用 @ 目标时不能再填写用户ID；请只填写金额".to_string());
        }
        return Ok((target, parse_positive_transfer_amount(amount)?));
    }

    let parts: Vec<&str> = args.split_whitespace().collect();
    if let [target, amount] = parts.as_slice() {
        return Ok((
            parse_command_target_subject_id(target)?,
            parse_positive_transfer_amount(amount)?,
        ));
    }
    if legacy_hyphen
        && let Some((target, amount)) = args.rsplit_once('-')
        && !target.trim().is_empty()
        && !amount.trim().is_empty()
    {
        return Ok((
            parse_command_target_subject_id(target.trim())?,
            parse_positive_transfer_amount(amount.trim())?,
        ));
    }
    Err(format!(
        "用法：{TRANSFER_USAGE}；旧格式为“转账 <用户ID>-<金额>”"
    ))
}

fn parse_gift_args(
    args: &str,
    mention: Option<String>,
    legacy_hyphen: bool,
) -> Result<(String, String, i64), String> {
    let args = args.trim();
    if let Some(target) = mention {
        if args.is_empty() {
            return Err(format!("用法：{GIFT_USAGE}"));
        }
        if args.split_whitespace().next() == Some(target.as_str()) {
            return Err("已使用 @ 目标时不能再填写用户ID；请删除重复目标".to_string());
        }
        let (item_name, quantity) = parse_item_quantity(args, legacy_hyphen, GIFT_USAGE)?;
        return Ok((target, item_name.to_string(), quantity));
    }

    if let Some((target, item_args)) = split_first_argument(args) {
        let target = parse_command_target_subject_id(target)?;
        if item_args.is_empty() {
            return Err(format!("用法：{GIFT_USAGE}"));
        }
        let (item_name, quantity) = parse_item_quantity(item_args, legacy_hyphen, GIFT_USAGE)?;
        return Ok((target, item_name.to_string(), quantity));
    }

    if legacy_hyphen
        && let Some((head, quantity_text)) = args.rsplit_once('-')
        && let Ok(quantity) = quantity_text.trim().parse::<i64>()
        && let Some((target, item_name)) = head.rsplit_once('-')
        && !target.trim().is_empty()
        && !item_name.trim().is_empty()
    {
        if !(1..=9999).contains(&quantity) {
            return Err("数量必须在 1 到 9999 之间".to_string());
        }
        return Ok((
            parse_command_target_subject_id(target.trim())?,
            parse_required_catalog_name(item_name, GIFT_USAGE)?.to_string(),
            quantity,
        ));
    }

    Err(format!(
        "用法：{GIFT_USAGE}；旧格式为“发送物品 <用户ID>-<物品名>-<数量>”"
    ))
}

fn split_first_argument(input: &str) -> Option<(&str, &str)> {
    let input = input.trim();
    let boundary = input
        .char_indices()
        .find(|(_, character)| character.is_whitespace())
        .map(|(index, _)| index)?;
    let target = &input[..boundary];
    let rest = input[boundary..].trim();
    (!target.is_empty()).then_some((target, rest))
}

fn parse_command_target_subject_id(value: &str) -> Result<String, String> {
    if value.chars().any(char::is_whitespace) {
        return Err("目标用户ID不能包含空白字符".to_string());
    }
    let value = parse_target_subject_id(value)?;
    if value == "all" {
        return Err("不能把 @全体 作为资产接收人".to_string());
    }
    Ok(value)
}

fn parse_positive_transfer_amount(value: &str) -> Result<i64, String> {
    let amount = value
        .parse::<i64>()
        .map_err(|_| "金额必须是正整数".to_string())?;
    if amount <= 0 {
        return Err("金额必须是正整数".to_string());
    }
    Ok(amount)
}

fn item_quality_label(quality: i64) -> String {
    format!("品质 {}", quality)
}

fn npc_kind_label(kind: &str) -> &'static str {
    match kind {
        "merchant" => "商人",
        "elder" => "长者",
        _ => "NPC",
    }
}

fn item_effect_description(item: &crate::store::ItemRecord) -> String {
    match item.effect_kind.as_str() {
        "restore_hp" => format!("恢复 {} 点生命", item.effect_amount),
        "restore_soul" => format!("恢复 {} 点魂力", item.effect_amount),
        "revive" => format!("复活并恢复 {}% 生命", item.revive_hp_percent),
        _ => item.description.clone(),
    }
}

fn display_direction(direction: &str) -> &'static str {
    match direction {
        "north" => "上",
        "south" => "下",
        "west" => "左",
        "east" => "右",
        _ => "未知",
    }
}

fn game_day_from_timestamp(timestamp: i64) -> Result<i64, String> {
    timestamp
        .checked_add(BEIJING_04_OFFSET_SECONDS)
        .ok_or_else(|| "系统时间戳超出可计算范围".to_string())
        .map(|shifted| shifted.div_euclid(SECONDS_PER_DAY))
}

fn current_unix_timestamp() -> Result<i64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("系统时间早于 Unix epoch：{error}"))
        .and_then(|duration| {
            i64::try_from(duration.as_secs()).map_err(|_| "系统时间戳超出 i64 范围".to_string())
        })
}

#[cfg(test)]
fn checkin_cycle_day(streak_days: i64) -> Result<i64, String> {
    if streak_days <= 0 {
        return Err("连签天数必须大于 0".to_string());
    }
    Ok((streak_days - 1).rem_euclid(7) + 1)
}

fn checkin_exp_reward(cycle_day: i64) -> Result<i64, String> {
    let index = usize::try_from(cycle_day - 1).map_err(|_| "签到轮次无效".to_string())?;
    CHECKIN_EXP_REWARDS
        .get(index)
        .copied()
        .ok_or_else(|| "签到轮次必须在 1 到 7 之间".to_string())
}

fn validate_context_kind_for_protocol(
    protocol: crate::message::Protocol,
    context_kind: &str,
) -> Result<(), String> {
    match (protocol, context_kind) {
        (_, "group") | (crate::message::Protocol::QqOfficial, "channel") => Ok(()),
        (crate::message::Protocol::OneBot11, "channel") => {
            Err("OneBot 11 当前只支持授权 group 上下文".to_string())
        }
        _ => Err("上下文类型只能是 group 或 channel".to_string()),
    }
}

fn parse_legacy_inspect_args(args: &str) -> Result<&str, String> {
    let mut parts = args.split_whitespace();
    let subject_id = parts.next().unwrap_or_default();
    if subject_id.is_empty() || parts.next().is_some() {
        return Err("用法：旧档检查 <用户ID>".to_string());
    }
    Ok(subject_id)
}

fn parse_legacy_claim_args(args: &str) -> Result<(&str, &str), String> {
    let mut parts = args.split_whitespace();
    let subject_id = parts.next().unwrap_or_default();
    let account_id = parts.next().unwrap_or_default();
    let confirmation = parts.next().unwrap_or_default();
    if subject_id.is_empty()
        || account_id.is_empty()
        || confirmation != "确认"
        || parts.next().is_some()
    {
        return Err("用法：旧档认领 <用户ID> <当前account_id> 确认".to_string());
    }
    Ok((subject_id, account_id))
}

fn parse_menu_page(args: &str) -> Result<usize, String> {
    let args = args.trim();
    if args.is_empty() {
        return Ok(0);
    }
    let mut parts = args.split_whitespace();
    let token = parts.next().unwrap_or_default();
    if parts.next().is_some() {
        return Err("用法：斗罗系统 [页码|开始|角色|世界|经济|任务|战斗]".to_string());
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
        .ok_or_else(|| "用法：斗罗系统 [页码|开始|角色|世界|经济|任务|战斗]".to_string())
}

fn parse_optional_log_limit(args: &str) -> Result<usize, String> {
    let args = args.trim();
    if args.is_empty() {
        return Ok(20);
    }
    let limit = args
        .parse::<usize>()
        .map_err(|_| "用法：战斗日志 [数量]".to_string())?;
    if !(1..=100).contains(&limit) {
        return Err("战斗日志数量必须在 1 到 100 之间".to_string());
    }
    Ok(limit)
}

fn battle_status_label(status: &str) -> &'static str {
    match status {
        "active" => "进行中",
        "won" => "胜利",
        "escaped" => "已逃跑",
        "defeated" => "战败",
        _ => "未知",
    }
}

fn soul_ring_color_label(color: &str) -> &'static str {
    match color {
        "white" => "白环",
        "yellow" => "黄环",
        "purple" => "紫环",
        "black" => "黑环",
        "red" => "红环",
        _ => "未知魂环",
    }
}

fn skill_proficiency_label(level: i64, proficiency: i64) -> String {
    if level >= MAX_SKILL_LEVEL {
        return format!("{proficiency}/{MAX_SKILL_PROFICIENCY}（满级）");
    }
    skill_proficiency_threshold(level + 1).ok().map_or_else(
        || proficiency.to_string(),
        |threshold| format!("{proficiency}/{threshold}"),
    )
}

fn skill_damage_modifier_label(modifier: &SkillDamageModifierRecord) -> String {
    let legacy = if modifier.rule_version == "legacy" {
        "（历史规则）"
    } else {
        ""
    };
    format!(
        "Lv.{} · {}%{legacy}",
        modifier.skill_level, modifier.damage_percent
    )
}

fn skill_effect_label(effect: &SkillEffectRecord) -> String {
    match (effect.effect_kind.as_str(), effect.target_kind.as_str()) {
        ("beast_attack_reduction", "enemy") => format!(
            "魂兽攻击 -{}% · {} 回合",
            effect.magnitude_percent, effect.duration_rounds
        ),
        _ => effect.description.clone(),
    }
}

fn battle_skill_effect_short_label(effect: &BattleSkillEffectRecord) -> String {
    match (effect.effect_kind.as_str(), effect.target_kind.as_str()) {
        ("beast_attack_reduction", "enemy") => {
            format!("魂兽攻击 -{}%", effect.magnitude_percent)
        }
        _ => effect.description.clone(),
    }
}

fn battle_skill_effect_label(effect: &BattleSkillEffectRecord) -> String {
    format!(
        "{} · {} 回合",
        battle_skill_effect_short_label(effect),
        effect.duration_rounds
    )
}

fn active_battle_skill_effect_label(
    effect: &BattleSkillEffectRecord,
    current_sequence: i64,
) -> String {
    let remaining = effect
        .expires_after_sequence
        .saturating_sub(current_sequence)
        .max(0);
    format!(
        "{} · {} · 剩余 {} 回合",
        effect.skill_name,
        battle_skill_effect_short_label(effect),
        remaining
    )
}

fn format_battle_event(event: &BattleEventRecord) -> String {
    let action = event
        .skill_name
        .as_ref()
        .map(|name| format!("魂技 {name}"))
        .unwrap_or_else(|| match event.event_kind.as_str() {
            "challenge" => "挑战".to_string(),
            "attack" => "攻击".to_string(),
            "flee" => "逃跑".to_string(),
            _ => "未知动作".to_string(),
        });
    let mut text = format!(
        "#{} · {} · 玩家 {}/{} · 魂兽 {}/{}",
        event.sequence,
        action,
        event.player_hp_before,
        event.player_hp_after,
        event.beast_hp_before,
        event.beast_hp_after
    );
    if event.player_damage > 0 || event.beast_damage > 0 {
        text.push_str(&format!(
            " · 伤害 {}/{}",
            event.player_damage, event.beast_damage
        ));
    }
    if let Some(success) = event.flee_success {
        text.push_str(if success {
            " · 逃跑成功"
        } else {
            " · 逃跑失败"
        });
    }
    text.push_str(&format!(" · {}", battle_status_label(&event.status_after)));
    text
}

#[cfg(test)]
mod tests {
    use abi_stable::std_types::RString;

    use super::*;

    fn command_request(command: &str, args: &str, message_id: &str) -> CommandRequest {
        CommandRequest {
            args: RString::from(args),
            command_name: RString::from(command),
            sender_id: RString::from("economy-user"),
            group_id: RString::new(),
            raw_event_json: RString::from(
                r#"{"self_id":"10001","qimen_context":{"version":1,"protocol":"onebot11","account_id":"10001"}}"#,
            ),
            sender_nickname: RString::from("经济测试"),
            message_id: RString::from(message_id),
            timestamp: 0,
        }
    }

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
    fn legacy_claim_arguments_require_explicit_account_confirmation() {
        assert_eq!(parse_legacy_inspect_args("legacy-user"), Ok("legacy-user"));
        assert!(parse_legacy_inspect_args("").is_err());
        assert!(parse_legacy_inspect_args("user extra").is_err());

        assert_eq!(
            parse_legacy_claim_args("legacy-user 10001 确认"),
            Ok(("legacy-user", "10001"))
        );
        for invalid in [
            "legacy-user 10001",
            "legacy-user 10001 取消",
            "legacy-user 10001 确认 extra",
        ] {
            assert!(parse_legacy_claim_args(invalid).is_err());
        }
    }

    #[test]
    fn authorized_context_arguments_keep_legacy_group_shape_but_require_revoke_confirmation() {
        assert_eq!(
            parse_grant_context_args("746339543"),
            Ok(("group".to_string(), "746339543".to_string(), String::new()))
        );
        assert_eq!(
            parse_grant_context_args("channel channel-1 史莱克学院"),
            Ok((
                "channel".to_string(),
                "channel-1".to_string(),
                "史莱克学院".to_string()
            ))
        );
        assert_eq!(
            parse_revoke_context_args("group 746339543 确认"),
            Ok(("group".to_string(), "746339543".to_string()))
        );
        assert!(parse_revoke_context_args("group 746339543").is_err());
        assert_eq!(parse_context_cursor(""), Ok(None));
        assert_eq!(parse_context_cursor("12"), Ok(Some(12)));
        assert!(parse_context_cursor("-1").is_err());
    }

    #[test]
    fn map_command_arguments_are_bounded_and_directional() {
        assert_eq!(parse_map_page(""), Ok(1));
        assert_eq!(parse_map_page("2"), Ok(2));
        assert!(parse_map_page("0").is_err());
        assert!(parse_map_page("1 extra").is_err());
        assert_eq!(parse_direction_arg("上"), Ok("上"));
        assert_eq!(parse_direction_arg("东"), Ok("东"));
        assert!(parse_direction_arg("").is_err());
        assert!(parse_direction_arg("前").is_err());
        assert!(parse_direction_arg("上 下").is_err());
        assert_eq!(parse_optional_map_name(""), Ok(None));
        assert_eq!(parse_optional_map_name("圣魂村"), Ok(Some("圣魂村")));
        assert!(parse_optional_map_name("圣魂村 多余").is_err());
    }

    #[test]
    fn checkin_rewards_use_beijing_four_oclock_game_day_and_legacy_formula() {
        assert_eq!(game_day_from_timestamp(0), Ok(0));
        assert_eq!(game_day_from_timestamp(19 * 3_600 + 59 * 60 + 59), Ok(0));
        assert_eq!(game_day_from_timestamp(20 * 3_600), Ok(1));
        assert_eq!(game_day_from_timestamp(20 * 3_600 + SECONDS_PER_DAY), Ok(2));

        let expected = [
            (1, 1, 60),
            (2, 2, 70),
            (3, 3, 80),
            (4, 4, 90),
            (5, 5, 100),
            (6, 6, 110),
            (7, 7, 150),
            (8, 1, 60),
        ];
        for (streak, cycle, exp) in expected {
            let computed_cycle = checkin_cycle_day(streak).expect("连签轮次应有效");
            assert_eq!(computed_cycle, cycle);
            assert_eq!(checkin_exp_reward(computed_cycle), Ok(exp));
        }
        assert!(checkin_cycle_day(0).is_err());
        assert!(checkin_exp_reward(0).is_err());
        assert_eq!(CHECKIN_CURRENCY_CODE, "gold_soul_coin");
        assert_eq!(CHECKIN_CURRENCY_NAME, "金魂币");
    }

    #[test]
    fn daily_checkin_command_persists_once_and_renders_duplicate_without_regranting() {
        let directory = tempfile::tempdir().expect("临时目录应创建");
        let store = Store::initialize(directory.path(), &crate::config::DatabaseConfig::default())
            .expect("数据库应初始化");
        let service = GameService::with_assets(
            store,
            PluginConfig::default(),
            IllustrationAssets::default(),
        );
        let request = |command: &str, args: &str, message_id: &str| CommandRequest {
            args: abi_stable::std_types::RString::from(args),
            command_name: abi_stable::std_types::RString::from(command),
            sender_id: abi_stable::std_types::RString::from("checkin-user"),
            group_id: abi_stable::std_types::RString::new(),
            raw_event_json: abi_stable::std_types::RString::from(
                r#"{"self_id":"10001","qimen_context":{"version":1,"protocol":"onebot11","account_id":"10001"}}"#,
            ),
            sender_nickname: abi_stable::std_types::RString::new(),
            message_id: abi_stable::std_types::RString::from(message_id),
            timestamp: 0,
        };
        service
            .register(&request("开始穿越", "签到测试 女", "register"))
            .expect("应创建签到测试角色");

        let initial_wallet = crate::message::render_text(
            &service
                .wallet(&request("钱包", "", "wallet-initial"))
                .expect("未签到角色应能查询零余额"),
        );
        assert!(initial_wallet.contains("金魂币：0"));
        assert!(
            service
                .wallet(&request("钱包", "extra", "wallet-invalid"))
                .is_err()
        );

        let first = crate::message::render_text(
            &service
                .daily_checkin(&request("签到", "", "checkin-first"))
                .expect("首签应成功"),
        );
        assert!(first.contains("结果：签到成功"));
        assert!(first.contains("经验奖励：+60"));
        assert!(first.contains("金魂币奖励：+"));

        let wallet = crate::message::render_text(
            &service
                .wallet(&request("余额", "", "wallet-after-checkin"))
                .expect("签到后应能查询余额"),
        );
        assert!(wallet.contains("金魂币："));
        assert!(wallet.contains("当前展示签到使用的金魂币余额"));

        let duplicate = crate::message::render_text(
            &service
                .daily_checkin(&request("打卡", "", "checkin-duplicate"))
                .expect("同日重复应返回领取凭据"),
        );
        assert!(duplicate.contains("结果：今日已签到"));
        assert!(duplicate.contains("（已领取）"));
        assert!(!duplicate.contains("经验奖励：+"));
        assert!(
            service
                .daily_checkin(&request("签到", "extra", "checkin-invalid"))
                .is_err()
        );

        let identity = resolve_identity(&request("状态", "", "status"), &service.config.identity)
            .expect("身份应解析");
        let key = service.identity_key(&identity, &identity.subject_id);
        assert_eq!(
            service
                .store
                .list_operation_logs(&key, None, 100)
                .expect("应读取操作日志")
                .entries
                .iter()
                .filter(|entry| entry.command == "签到")
                .count(),
            1
        );
    }

    #[test]
    fn allowlist_gates_game_commands_per_bot_context_but_not_private_chat() {
        let directory = tempfile::tempdir().expect("临时目录应创建");
        let store = Store::initialize(directory.path(), &crate::config::DatabaseConfig::default())
            .expect("数据库应初始化");
        let mut config = PluginConfig::default();
        config.authorization.mode = AuthorizationMode::Allowlist;
        let service = GameService::with_assets(store, config, IllustrationAssets::default());
        let request = |command: &str, args: &str, group_id: &str, sender_id: &str| CommandRequest {
            args: abi_stable::std_types::RString::from(args),
            command_name: abi_stable::std_types::RString::from(command),
            sender_id: abi_stable::std_types::RString::from(sender_id),
            group_id: abi_stable::std_types::RString::from(group_id),
            raw_event_json: abi_stable::std_types::RString::from(
                r#"{"self_id":10001,"qimen_context":{"version":1,"protocol":"onebot11","account_id":"10001"}}"#,
            ),
            sender_nickname: abi_stable::std_types::RString::new(),
            message_id: abi_stable::std_types::RString::from("message-auth"),
            timestamp: 0,
        };
        let group_request = request("状态", "", "group-1", "player-user");
        assert!(service.ensure_context_authorized(&group_request).is_err());
        assert!(
            service
                .ensure_context_authorized(&request("状态", "", "", "player-user"))
                .is_ok()
        );

        let grant = request("授权上下文", "group group-1 测试群", "", "owner-user");
        assert!(service.grant_context(&grant).is_ok());
        assert!(service.ensure_context_authorized(&group_request).is_ok());
        let listed = crate::message::render_text(
            &service
                .list_contexts(&request("查看授权", "", "", "owner-user"))
                .expect("应列出授权上下文"),
        );
        assert!(listed.contains("group-1"));
        assert!(listed.contains("测试群"));

        let revoke = request("取消授权", "group group-1 确认", "", "owner-user");
        assert!(service.revoke_context(&revoke).is_ok());
        assert!(service.ensure_context_authorized(&group_request).is_err());
    }

    #[test]
    fn legacy_owner_workflow_uses_current_resolved_account() {
        let directory = tempfile::tempdir().expect("临时目录应创建");
        let store = Store::initialize(directory.path(), &crate::config::DatabaseConfig::default())
            .expect("数据库应初始化");
        let service = GameService::with_assets(
            store,
            PluginConfig::default(),
            IllustrationAssets::default(),
        );
        let request = |args: &str| CommandRequest {
            args: abi_stable::std_types::RString::from(args),
            command_name: abi_stable::std_types::RString::from("旧档检查"),
            sender_id: abi_stable::std_types::RString::from("owner-user"),
            group_id: abi_stable::std_types::RString::new(),
            raw_event_json: abi_stable::std_types::RString::from(
                r#"{"self_id":"0010001","qimen_context":{"version":1,"protocol":"onebot11","account_id":"0010001"}}"#,
            ),
            sender_nickname: abi_stable::std_types::RString::new(),
            message_id: abi_stable::std_types::RString::from("message-1"),
            timestamp: 0,
        };

        let inspection = service
            .inspect_legacy(&request("legacy-user"))
            .expect("缺失旧档也应返回检查结果");
        let inspection_text = crate::message::render_text(&inspection);
        assert!(inspection_text.contains("当前账号：0010001"));
        assert!(inspection_text.contains("状态：未找到"));

        assert!(
            service
                .claim_legacy(&request("legacy-user 10001 确认"))
                .expect_err("确认账号不一致必须拒绝")
                .contains("不一致")
        );
        let result = service
            .claim_legacy(&request("legacy-user 0010001 确认"))
            .expect("正确确认但没有旧档时应返回未找到");
        assert!(crate::message::render_text(&result).contains("结果：未找到"));
    }

    #[test]
    fn operation_log_records_only_bounded_command_metadata() {
        let directory = tempfile::tempdir().expect("临时目录应创建");
        let store = Store::initialize(directory.path(), &crate::config::DatabaseConfig::default())
            .expect("数据库应初始化");
        let service = GameService::with_assets(
            store,
            PluginConfig::default(),
            IllustrationAssets::default(),
        );
        let request = CommandRequest {
            args: abi_stable::std_types::RString::from("唐小三 男"),
            command_name: abi_stable::std_types::RString::from("开始穿越"),
            sender_id: abi_stable::std_types::RString::from("user-log"),
            group_id: abi_stable::std_types::RString::from("group-log"),
            raw_event_json: abi_stable::std_types::RString::from(
                r#"{"self_id":10001,"qimen_context":{"version":1,"protocol":"onebot11","account_id":"10001"},"message":"secret raw body"}"#,
            ),
            sender_nickname: abi_stable::std_types::RString::from("private nickname"),
            message_id: abi_stable::std_types::RString::from("message-log"),
            timestamp: 0,
        };
        service
            .register(&request)
            .expect("角色与操作日志应原子写入");
        let identity = resolve_identity(&request, &service.config.identity).expect("身份应解析");
        let key = service.identity_key(&identity, &identity.subject_id);
        let page = service
            .store
            .list_operation_logs(&key, None, 10)
            .expect("日志应读取");
        assert_eq!(page.entries.len(), 1);
        let entry = &page.entries[0];
        assert_eq!(entry.command, "开始穿越");
        assert_eq!(entry.outcome, "ok");
        assert_eq!(entry.source_message_id, "message-log");
        assert_eq!(entry.details_json, r#"{"context":"group","has_args":true}"#);
        for secret in [
            "唐小三",
            "secret raw body",
            "private nickname",
            "raw_event_json",
        ] {
            assert!(!entry.details_json.contains(secret));
        }
    }

    #[test]
    fn mutation_rolls_back_when_atomic_audit_is_invalid() {
        let directory = tempfile::tempdir().expect("临时目录应创建");
        let store = Store::initialize(directory.path(), &crate::config::DatabaseConfig::default())
            .expect("数据库应初始化");
        let service = GameService::with_assets(
            store,
            PluginConfig::default(),
            IllustrationAssets::default(),
        );
        let mut request = CommandRequest {
            args: abi_stable::std_types::RString::from("唐小三 男"),
            command_name: abi_stable::std_types::RString::from(" bad-command"),
            sender_id: abi_stable::std_types::RString::from("atomic-user"),
            group_id: abi_stable::std_types::RString::new(),
            raw_event_json: abi_stable::std_types::RString::from(r#"{"self_id":10001}"#),
            sender_nickname: abi_stable::std_types::RString::new(),
            message_id: abi_stable::std_types::RString::from("message-atomic"),
            timestamp: 0,
        };
        assert!(service.register(&request).is_err());

        request.command_name = abi_stable::std_types::RString::from("状态");
        request.args = abi_stable::std_types::RString::new();
        assert!(
            service
                .status(&request)
                .expect_err("日志写入失败时角色创建也必须回滚")
                .contains("还没有角色")
        );
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
        assert!(
            MENU_PAGES[second]
                .entries
                .iter()
                .any(|entry| entry.command == "签到")
        );
        assert!(
            MENU_PAGES[second]
                .entries
                .iter()
                .any(|entry| entry.command == "钱包")
        );
        assert!(
            MENU_PAGES[second]
                .entries
                .iter()
                .any(|entry| entry.command == "技能")
        );
        assert_eq!(parse_menu_page("世界").expect("世界分类应有效"), 2);
        assert_eq!(parse_menu_page("经济").expect("经济分类应有效"), 3);
        assert_eq!(parse_menu_page("任务").expect("任务分类应有效"), 4);
        assert_eq!(parse_menu_page("战斗").expect("战斗分类应有效"), 5);
        assert!(parse_menu_page("7").is_err());
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
        assert!(second.contains("技能：查看已学习魂技和魂力消耗"));
        assert!(second.contains("签到：领取每日经验和金魂币"));
        assert!(second.contains("钱包：查看金魂币余额"));
        assert!(second.contains("斗罗系统 1"));
        assert!(second.contains("斗罗系统 3"));

        let third = crate::message::render_text(&service.menu("世界").expect("世界页应有效"));
        assert!(third.contains("位置：查看当前地图"));
        assert!(third.contains("掉落 [页码]：查看当前地图可拾取的地面掉落"));
        assert!(third.contains("拾取 <掉落ID>：拾取当前地图的一整堆物品"));
        assert!(third.contains("斗罗系统 2"));
        assert!(third.contains("斗罗系统 4"));

        let fourth = crate::message::render_text(&service.menu("经济").expect("经济页应有效"));
        assert!(fourth.contains("NPC：查看当前地图的 NPC"));
        assert!(fourth.contains("购买 <物品> [数量]：从当前商店购买物品"));
        assert!(fourth.contains("使用 <物品>：使用背包中的消耗品"));
        assert!(fourth.contains("斗罗系统 3"));

        let fifth = crate::message::render_text(&service.menu("任务").expect("任务页应有效"));
        assert!(fifth.contains("任务 [页码]：查看当前地图可接取的任务"));
        assert!(fifth.contains("提交任务 <任务>：提交已完成任务并领取奖励"));
        assert!(fifth.contains("斗罗系统 4"));

        let sixth = crate::message::render_text(&service.menu("战斗").expect("战斗页应有效"));
        assert!(sixth.contains("魂兽 [页码]：查看当前地图可挑战的魂兽"));
        assert!(sixth.contains("挑战 <魂兽>：发起一场魂兽挑战"));
        assert!(sixth.contains("攻击：按武魂修正进行普通攻击并承受魂兽反击"));
        assert!(sixth.contains("斗罗系统 5"));
    }

    #[test]
    fn economy_command_arguments_keep_new_and_legacy_shapes() {
        assert_eq!(parse_single_page("", "背包"), Ok(1));
        assert_eq!(parse_single_page("2", "背包"), Ok(2));
        assert!(parse_single_page("0", "背包").is_err());
        assert!(parse_single_page("2 extra", "背包").is_err());
        assert_eq!(parse_ground_drop_id("42"), Ok(42));
        assert!(parse_ground_drop_id("").is_err());
        assert!(parse_ground_drop_id("0").is_err());
        assert!(parse_ground_drop_id("42 extra").is_err());

        assert_eq!(parse_shop_args(""), Ok((None, 1)));
        assert_eq!(parse_shop_args("2"), Ok((None, 2)));
        assert_eq!(parse_shop_args("杂货商人"), Ok((Some("杂货商人"), 1)));
        assert_eq!(parse_shop_args("杂货商人 2"), Ok((Some("杂货商人"), 2)));
        assert!(parse_shop_args("杂货商人 2 extra").is_err());

        assert_eq!(
            parse_item_quantity("小回复药 2", true, "购买 <物品> [数量]"),
            Ok(("小回复药", 2))
        );
        assert_eq!(
            parse_item_quantity("小回复药-2", true, "购买 <物品> [数量]"),
            Ok(("小回复药", 2))
        );
        assert_eq!(
            parse_item_quantity("小回复药-2", false, "购买 <物品> [数量]"),
            Ok(("小回复药-2", 1))
        );
        assert_eq!(
            parse_item_quantity("魂力 恢复药 3", true, "购买 <物品> [数量]"),
            Ok(("魂力 恢复药", 3))
        );
        assert!(parse_item_quantity("小回复药 0", true, "购买").is_err());
        assert!(parse_item_quantity("", true, "购买").is_err());
        assert_eq!(
            parse_required_catalog_name(" 村长 ", "对话 <NPC>"),
            Ok("村长")
        );
    }

    #[test]
    fn transfer_and_gift_arguments_preserve_string_ids_and_reject_ambiguity() {
        assert_eq!(
            parse_transfer_args("openid-with-dash 12", None, true),
            Ok(("openid-with-dash".to_string(), 12))
        );
        assert_eq!(
            parse_transfer_args("00123-7", None, true),
            Ok(("00123".to_string(), 7))
        );
        assert_eq!(
            parse_transfer_args("7", Some("member-openid".to_string()), true),
            Ok(("member-openid".to_string(), 7))
        );
        assert!(parse_transfer_args("member-openid 7", Some("other".to_string()), true).is_err());
        assert!(parse_transfer_args("member-openid 0", None, true).is_err());
        assert!(parse_transfer_args("member-openid-7", None, false).is_err());

        assert_eq!(
            parse_gift_args("member-openid 小回复药 2", None, true),
            Ok(("member-openid".to_string(), "小回复药".to_string(), 2))
        );
        assert_eq!(
            parse_gift_args("member-openid-小回复药-2", None, true),
            Ok(("member-openid".to_string(), "小回复药".to_string(), 2))
        );
        assert_eq!(
            parse_gift_args("小回复药 1", Some("member-openid".to_string()), true),
            Ok(("member-openid".to_string(), "小回复药".to_string(), 1))
        );
        assert!(
            parse_gift_args(
                "member-openid 小回复药",
                Some("member-openid".to_string()),
                true
            )
            .is_err()
        );
        assert!(parse_gift_args("member-openid-小回复药-0", None, true).is_err());
    }

    #[test]
    fn command_target_ids_cannot_be_whitespace_or_all() {
        assert!(parse_command_target_subject_id("member openid").is_err());
        assert!(parse_command_target_subject_id("all").is_err());
        assert_eq!(
            parse_command_target_subject_id("00-member-id"),
            Ok("00-member-id".to_string())
        );
    }

    #[test]
    fn economy_commands_render_the_complete_npc_shop_inventory_flow() {
        let directory = tempfile::tempdir().expect("临时目录应创建");
        let store = Store::initialize(directory.path(), &crate::config::DatabaseConfig::default())
            .expect("数据库应初始化");
        let service = GameService::with_assets(
            store,
            PluginConfig::default(),
            IllustrationAssets::default(),
        );
        service
            .register(&command_request(
                "开始穿越",
                "经济命令 男",
                "economy-register",
            ))
            .expect("应创建经济测试角色");
        service
            .daily_checkin(&command_request("签到", "", "economy-checkin"))
            .expect("应获得购买资金");

        let drops = crate::message::render_text(
            &service
                .ground_drops(&command_request("掉落", "", "economy-drops"))
                .expect("应读取当前地图掉落"),
        );
        assert!(drops.contains("当前地图没有可拾取的地面掉落"));

        let npcs = crate::message::render_text(
            &service
                .npcs(&command_request("NPC", "", "economy-npcs"))
                .expect("应列出当前 NPC"),
        );
        assert!(npcs.contains("村长"));
        assert!(npcs.contains("杂货商人"));

        let talk = crate::message::render_text(
            &service
                .talk(&command_request("对话", "杂货商人", "economy-talk"))
                .expect("应与商人对话"),
        );
        assert!(talk.contains("这里有旅途中用得上的药剂"));
        assert!(talk.contains("商店"));

        let shop_document = service
            .shop(&command_request("商店", "", "economy-shop"))
            .expect("应打开当前商店");
        let shop = crate::message::render_text(&shop_document);
        assert!(shop.contains("小回复药 · 10 金魂币"));
        assert!(shop.contains("魂力恢复药"));
        let markdown = crate::message::render_markdown(&shop_document, None);
        assert!(markdown.contains("# 杂货商人的商店"));
        assert!(markdown.contains("购买 \\<物品\\> \\[数量\\]"));

        let purchase = crate::message::render_text(
            &service
                .buy(&command_request("购买", "小回复药 2", "economy-buy"))
                .expect("应购买恢复药"),
        );
        assert!(purchase.contains("小回复药 x2"));
        assert!(purchase.contains("20 金魂币"));

        let inventory = crate::message::render_text(
            &service
                .inventory(&command_request("背包", "", "economy-inventory"))
                .expect("应读取背包"),
        );
        assert!(inventory.contains("小回复药 x2"));
        assert!(inventory.contains("恢复 50 点生命"));

        let full = crate::message::render_text(
            &service
                .use_item(&command_request("使用", "小回复药", "economy-use-full"))
                .expect("满生命应保留药剂"),
        );
        assert!(full.contains("当前属性已满，物品未消耗"));
        assert!(full.contains("背包数量：2"));

        let sale = crate::message::render_text(
            &service
                .sell(&command_request("出售", "小回复药-1", "economy-sell"))
                .expect("旧横线格式应可出售"),
        );
        assert!(sale.contains("小回复药 x1"));
        assert!(sale.contains("背包数量：1"));
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
    fn map_list_move_and_teleport_render_from_world_contract() {
        let directory = tempfile::tempdir().expect("临时目录应创建");
        let store = Store::initialize(directory.path(), &crate::config::DatabaseConfig::default())
            .expect("数据库应初始化");
        let mut config = PluginConfig::default();
        config.illustrations.mode = crate::config::IllustrationMode::Remote;
        config.illustrations.remote_base_url = "https://media.example.com/douluo".to_string();
        let service = GameService::with_assets(store, config, IllustrationAssets::default());
        let request = |command: &str, args: &str, message_id: &str| CommandRequest {
            args: abi_stable::std_types::RString::from(args),
            command_name: abi_stable::std_types::RString::from(command),
            sender_id: abi_stable::std_types::RString::from("world-user"),
            group_id: abi_stable::std_types::RString::new(),
            raw_event_json: abi_stable::std_types::RString::from(
                r#"{"self_id":"10001","qimen_context":{"version":1,"protocol":"onebot11","account_id":"10001"}}"#,
            ),
            sender_nickname: abi_stable::std_types::RString::new(),
            message_id: abi_stable::std_types::RString::from(message_id),
            timestamp: 0,
        };
        service
            .register(&request("开始穿越", "旅行者 女", "world-register"))
            .expect("应创建旅行角色");

        let location = crate::message::render_text(
            &service
                .location(&request("位置", "", "world-location"))
                .expect("位置应可查询"),
        );
        assert!(location.contains("方向：上：天斗帝国主城"));
        assert!(location.contains("下：西尔维斯"));
        assert!(location.contains("传送阵：可用"));

        let first_page = crate::message::render_text(
            &service
                .map_list(&request("地图列表", "", "world-list-1"))
                .expect("地图第一页应可查询"),
        );
        assert!(first_page.contains("页码：1 / 2"));
        assert!(first_page.contains("圣魂村 · Lv.1"));
        assert!(first_page.contains("地图列表 2"));
        let second_page = crate::message::render_text(
            &service
                .map_list(&request("地图列表", "2", "world-list-2"))
                .expect("地图第二页应可查询"),
        );
        assert!(second_page.contains("星斗中心 · Lv.20"));
        assert!(second_page.contains("等级不足"));

        let moved = service
            .move_direction(&request("向", "上", "world-move"))
            .expect("向上应成功移动");
        let moved_text = crate::message::render_text(&moved);
        assert!(moved_text.contains("当前位置 · 天斗帝国主城"));
        assert!(moved_text.contains("结果：移动成功"));
        assert!(moved.has_illustration());

        let teleported = service
            .teleport(&request("传送", "圣魂村", "world-teleport"))
            .expect("应传送回圣魂村");
        let teleported_text = crate::message::render_text(&teleported);
        assert!(teleported_text.contains("当前位置 · 圣魂村"));
        assert!(teleported_text.contains("结果：传送成功"));
        assert!(teleported.has_illustration());

        let identity = resolve_identity(
            &request("状态", "", "world-status"),
            &service.config.identity,
        )
        .expect("身份应解析");
        let key = service.identity_key(&identity, &identity.subject_id);
        let logs = service
            .store
            .list_operation_logs(&key, None, 100)
            .expect("旅行日志应可读取");
        assert_eq!(
            logs.entries
                .iter()
                .filter(|entry| entry.command == "向")
                .count(),
            1
        );
        assert_eq!(
            logs.entries
                .iter()
                .filter(|entry| entry.command == "传送")
                .count(),
            1
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

    #[test]
    fn task_commands_render_the_full_lifecycle() {
        let directory = tempfile::tempdir().expect("应创建临时目录");
        let store = Store::initialize(directory.path(), &crate::config::DatabaseConfig::default())
            .expect("数据库应初始化");
        let service = GameService::with_assets(
            store,
            PluginConfig::default(),
            IllustrationAssets::default(),
        );
        service
            .register(&command_request("开始穿越", "任务测试 男", "task-register"))
            .expect("应创建任务测试角色");
        service
            .awaken(&command_request("武魂觉醒", "", "task-awaken"))
            .expect("应完成任务测试角色觉醒");
        let list = crate::message::render_text(
            &service
                .quests(&command_request("任务", "", "task-list"))
                .expect("应渲染任务列表"),
        );
        assert!(list.contains("初入圣魂村"));
        let accepted = crate::message::render_text(
            &service
                .accept_quest(&command_request("接取任务", "初入圣魂村", "task-accept"))
                .expect("应接取任务"),
        );
        assert!(accepted.contains("接取任务成功"));
        let progress = crate::message::render_text(
            &service
                .quest_progress(&command_request("任务进度", "", "task-progress"))
                .expect("应渲染任务进度"),
        );
        assert!(progress.contains("初入圣魂村"));
        let submitted = crate::message::render_text(
            &service
                .submit_quest(&command_request("提交任务", "初入圣魂村", "task-submit"))
                .expect("应提交任务"),
        );
        assert!(submitted.contains("提交任务成功"));
        assert!(submitted.contains("经验"));
    }
}
