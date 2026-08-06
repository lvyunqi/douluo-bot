use abi_stable_host_api::CommandRequest;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::assets::IllustrationAssets;
use crate::catalog;
use crate::config::{AuthorizationMode, IllustrationMode, PluginConfig};
use crate::context::resolve_conversation_context;
use crate::identity::{ResolvedIdentity, resolve_identity, resolve_protocol};
use crate::message::{GameDocument, Illustration};
use crate::store::{
    AuthorizedContextChange, DailyCheckinInput, DailyCheckinResult, IdentityKey, LegacyClaimActor,
    LegacyClaimResult, LegacyIdentityState, OperationLogInput, PlayerStatus, Store,
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
                command: "状态",
                description: "查看角色属性、武魂和位置",
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
        entries: &[MenuEntry {
            command: "位置",
            description: "查看当前地图",
        }],
    },
];

const CHECKIN_CURRENCY_CODE: &str = "gold_soul_coin";
const CHECKIN_CURRENCY_NAME: &str = "金魂币";
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

    /// 按北京时间每日 04:00 划分游戏日，并由 Store 在单事务内发放奖励。
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
        assert!(second.contains("签到：领取每日经验和金魂币"));
        assert!(second.contains("钱包：查看金魂币余额"));
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
