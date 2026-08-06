//! QimenBot 动态插件入口。

mod assets;
mod catalog;
pub mod config;
mod context;
mod game;
mod identity;
pub mod message;
mod store;

use std::path::Path;
use std::sync::{Arc, OnceLock, RwLock};

use abi_stable_host_api::{
    CommandRequest, CommandResponse, PluginConfigRequest, PluginConfigResult, PluginInitConfig,
    PluginInitResult,
};
use qimen_dynamic_plugin_derive::dynamic_plugin;

use crate::assets::IllustrationAssets;
use crate::config::parse_config;
use crate::game::GameService;
use crate::message::{GameDocument, response_for};
use crate::store::Store;

static RUNTIME: OnceLock<RwLock<Option<Arc<GameService>>>> = OnceLock::new();

fn runtime_slot() -> &'static RwLock<Option<Arc<GameService>>> {
    RUNTIME.get_or_init(|| RwLock::new(None))
}

fn initialize(config: PluginInitConfig) -> Result<(), String> {
    let parsed = parse_config(config.config_json.as_str())?;
    catalog::validate_embedded_manifest()?;
    let data_dir = config.data_dir.as_str().trim();
    if data_dir.is_empty() {
        return Err("QimenBot 未提供 data_dir，无法安全创建游戏数据库".to_string());
    }
    let data_dir = Path::new(data_dir);
    let store = Store::initialize(data_dir, &parsed.database)?;
    let illustration_assets = IllustrationAssets::load(data_dir, &parsed.illustrations)?;
    let service = Arc::new(GameService::with_assets(store, parsed, illustration_assets));
    let mut slot = runtime_slot()
        .write()
        .map_err(|_| "插件运行时锁已损坏".to_string())?;
    *slot = Some(service);
    Ok(())
}

fn shutdown_runtime() {
    if let Ok(mut slot) = runtime_slot().write() {
        *slot = None;
    }
}

fn with_service(
    req: &CommandRequest,
    audit_success: bool,
    require_authorized_context: bool,
    operation: impl FnOnce(&GameService) -> Result<GameDocument, String>,
) -> CommandResponse {
    let service = match runtime_slot().read() {
        Ok(slot) => slot.clone(),
        Err(_) => return CommandResponse::text("插件运行时锁已损坏，请联系管理员"),
    };
    let Some(service) = service else {
        return CommandResponse::text("斗罗大陆插件尚未完成初始化，请联系管理员");
    };
    let (result, denied) =
        execute_service_operation(&service, req, require_authorized_context, operation);
    let outcome = if denied {
        "denied"
    } else if result.is_ok() {
        "ok"
    } else {
        "error"
    };
    // 成功写操作在业务事务内审计；只读命令与失败尝试使用尽力写入。
    if audit_success || result.is_err() {
        drop(service.record_operation(req, outcome));
    }
    let document = match result {
        Ok(document) => document,
        Err(error) => GameDocument::new("操作失败").line(error),
    };
    response_for(
        req,
        &document,
        service.message_config(),
        service.illustration_config(),
    )
}

fn execute_service_operation(
    service: &GameService,
    req: &CommandRequest,
    require_authorized_context: bool,
    operation: impl FnOnce(&GameService) -> Result<GameDocument, String>,
) -> (Result<GameDocument, String>, bool) {
    let authorization = if require_authorized_context {
        service.ensure_context_authorized(req)
    } else {
        Ok(())
    };
    let denied = authorization.is_err();
    let result = authorization.and_then(|()| operation(service));
    (result, denied)
}

#[dynamic_plugin(
    id = "douluo-game",
    version = "0.1.0",
    api = "0.6",
    config_schema = "../config.schema.json",
    config_ui = "../config.ui.json",
    config_version = 4,
    config_apply = "reload"
)]
mod plugin {
    use super::*;

    /// 初始化数据库和游戏服务；重复加载会用新状态原子替换旧状态。
    #[init]
    fn init(config: PluginInitConfig) -> PluginInitResult {
        match initialize(config) {
            Ok(()) => PluginInitResult::ok(),
            Err(error) => PluginInitResult::err(&error),
        }
    }

    /// 在线保存前只校验配置，不写文件、不启动线程。
    #[validate_config]
    fn validate(request: &PluginConfigRequest) -> PluginConfigResult {
        match parse_config(request.config_json.as_str()) {
            Ok(_) => PluginConfigResult::ok(),
            Err(error) => PluginConfigResult::err(&error),
        }
    }

    #[shutdown]
    fn shutdown() {
        shutdown_runtime();
    }

    #[command(
        name = "斗罗系统",
        description = "查看斗罗大陆游戏菜单（支持页码或分类）",
        aliases = "斗罗菜单,菜单",
        category = "斗罗大陆·导航",
        scope = "all"
    )]
    fn menu(req: &CommandRequest) -> CommandResponse {
        with_service(req, true, true, |service| service.menu(req.args.as_str()))
    }

    #[command(
        name = "开始穿越",
        description = "创建斗罗大陆角色：开始穿越 <角色名> <男|女>",
        aliases = "开始转生",
        category = "斗罗大陆·角色",
        scope = "all"
    )]
    fn register(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.register(req))
    }

    #[command(
        name = "武魂觉醒",
        description = "觉醒第一武魂",
        aliases = "觉醒",
        category = "斗罗大陆·角色",
        scope = "all"
    )]
    fn awaken(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.awaken(req))
    }

    #[command(
        name = "开武魂",
        description = "开启当前武魂并进入战斗形态",
        aliases = "武魂开启",
        category = "斗罗大陆·角色",
        scope = "all"
    )]
    fn open_wuhun(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.open_wuhun(req))
    }

    #[command(
        name = "关武魂",
        description = "关闭当前武魂并退出战斗形态",
        aliases = "武魂关闭",
        category = "斗罗大陆·角色",
        scope = "all"
    )]
    fn close_wuhun(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.close_wuhun(req))
    }

    #[command(
        name = "技能",
        description = "查看已学习魂技和魂力消耗",
        aliases = "魂技,技能列表",
        category = "斗罗大陆·角色",
        scope = "all"
    )]
    fn skills(req: &CommandRequest) -> CommandResponse {
        with_service(req, true, true, |service| service.skills(req))
    }

    #[command(
        name = "技能详情",
        description = "查看已学习魂技的详细属性：技能详情 <魂技>",
        category = "斗罗大陆·角色",
        scope = "all"
    )]
    fn skill_detail(req: &CommandRequest) -> CommandResponse {
        with_service(req, true, true, |service| service.skill_detail(req))
    }

    #[command(
        name = "魂环",
        description = "查看已吸收魂环和待吸收魂环",
        aliases = "查看魂环",
        category = "斗罗大陆·角色",
        scope = "all"
    )]
    fn soul_rings(req: &CommandRequest) -> CommandResponse {
        with_service(req, true, true, |service| service.soul_rings(req))
    }

    #[command(
        name = "吸收魂环",
        description = "吸收击杀魂兽留下的魂环：吸收魂环 <魂兽>",
        aliases = "附加魂环",
        category = "斗罗大陆·战斗",
        scope = "all"
    )]
    fn absorb_soul_ring(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.absorb_soul_ring(req))
    }

    #[command(
        name = "释放技能",
        description = "在魂兽战斗中释放魂技：释放技能 <魂技>",
        aliases = "使用技能,使用魂技,施放魂技",
        category = "斗罗大陆·战斗",
        scope = "all"
    )]
    fn use_skill(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.use_skill(req))
    }

    #[command(
        name = "签到",
        description = "领取每日经验和金魂币",
        aliases = "每日签到,打卡",
        category = "斗罗大陆·角色",
        scope = "all"
    )]
    fn daily_checkin(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.daily_checkin(req))
    }

    #[command(
        name = "钱包",
        description = "查看金魂币余额",
        aliases = "我的钱包,余额",
        category = "斗罗大陆·角色",
        scope = "all"
    )]
    fn wallet(req: &CommandRequest) -> CommandResponse {
        with_service(req, true, true, |service| service.wallet(req))
    }

    #[command(
        name = "转账",
        description = "向同一协议、Bot account_id 和 namespace 内的玩家转账：转账 <用户ID> <金额>",
        aliases = "转钱,汇款",
        category = "斗罗大陆·经济",
        scope = "all"
    )]
    fn transfer(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.transfer_gold(req))
    }

    #[command(
        name = "NPC",
        description = "查看当前地图的 NPC",
        aliases = "人物,当前NPC",
        category = "斗罗大陆·经济",
        scope = "all"
    )]
    fn npcs(req: &CommandRequest) -> CommandResponse {
        with_service(req, true, true, |service| service.npcs(req))
    }

    #[command(
        name = "对话",
        description = "与当前地图的 NPC 对话：对话 <NPC>",
        aliases = "交谈,聊天",
        category = "斗罗大陆·经济",
        scope = "all"
    )]
    fn talk(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.talk(req))
    }

    #[command(
        name = "商店",
        description = "查看已对话商人的商品：商店 [页码]",
        aliases = "店铺",
        category = "斗罗大陆·经济",
        scope = "all"
    )]
    fn shop(req: &CommandRequest) -> CommandResponse {
        with_service(req, true, true, |service| service.shop(req))
    }

    #[command(
        name = "背包",
        description = "分页查看随身物品：背包 [页码]",
        aliases = "随身物品,物品,道具",
        category = "斗罗大陆·经济",
        scope = "all"
    )]
    fn inventory(req: &CommandRequest) -> CommandResponse {
        with_service(req, true, true, |service| service.inventory(req))
    }

    #[command(
        name = "购买",
        description = "从当前商店购买物品：购买 <物品> [数量]",
        aliases = "购买物品,买",
        category = "斗罗大陆·经济",
        scope = "all"
    )]
    fn buy(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.buy(req))
    }

    #[command(
        name = "出售",
        description = "向当前商店出售物品：出售 <物品> [数量]",
        aliases = "卖出,卖",
        category = "斗罗大陆·经济",
        scope = "all"
    )]
    fn sell(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.sell(req))
    }

    #[command(
        name = "使用",
        description = "使用背包中的物品：使用 <物品>",
        aliases = "使用物品,用",
        category = "斗罗大陆·经济",
        scope = "all"
    )]
    fn use_item(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.use_item(req))
    }

    #[command(
        name = "发送物品",
        description = "向同一协议、Bot account_id 和 namespace 内的玩家赠送物品：发送物品 <用户ID> <物品> [数量]",
        aliases = "赠送,赠送物品",
        category = "斗罗大陆·经济",
        scope = "all"
    )]
    fn gift_item(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.gift_item(req))
    }

    #[command(
        name = "状态",
        description = "查看自己的角色状态",
        aliases = "我的状态,属性",
        category = "斗罗大陆·角色",
        scope = "all"
    )]
    fn status(req: &CommandRequest) -> CommandResponse {
        with_service(req, true, true, |service| service.status(req))
    }

    #[command(
        name = "位置",
        description = "查看当前地图",
        aliases = "地图,当前位置",
        category = "斗罗大陆·世界",
        scope = "all"
    )]
    fn location(req: &CommandRequest) -> CommandResponse {
        with_service(req, true, true, |service| service.location(req))
    }

    #[command(
        name = "地图列表",
        description = "分页查看地图、等级要求和传送阵",
        aliases = "地图清单",
        category = "斗罗大陆·世界",
        scope = "all"
    )]
    fn map_list(req: &CommandRequest) -> CommandResponse {
        with_service(req, true, true, |service| service.map_list(req))
    }

    #[command(
        name = "向",
        description = "沿当前地图出口移动：向 <上|下|左|右>",
        category = "斗罗大陆·世界",
        scope = "all"
    )]
    fn move_direction(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.move_direction(req))
    }

    #[command(
        name = "传送",
        description = "使用传送阵：传送 [地图名称]",
        category = "斗罗大陆·世界",
        scope = "all"
    )]
    fn teleport(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.teleport(req))
    }

    #[command(
        name = "掉落",
        description = "分页查看当前地图可拾取的地面掉落：掉落 [页码]",
        aliases = "查看掉落,地面掉落",
        category = "斗罗大陆·世界",
        scope = "all"
    )]
    fn ground_drops(req: &CommandRequest) -> CommandResponse {
        with_service(req, true, true, |service| service.ground_drops(req))
    }

    #[command(
        name = "拾取",
        description = "拾取当前地图的完整掉落堆：拾取 <掉落ID>",
        aliases = "捡取,拾取物品",
        category = "斗罗大陆·世界",
        scope = "all"
    )]
    fn pick_up_ground_drop(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.pick_up_ground_drop(req))
    }

    #[command(
        name = "任务",
        description = "查看当前地图可接取的任务：任务 [页码]",
        aliases = "任务列表,任务清单",
        category = "斗罗大陆·任务",
        scope = "all"
    )]
    fn quests(req: &CommandRequest) -> CommandResponse {
        with_service(req, true, true, |service| service.quests(req))
    }

    #[command(
        name = "接取任务",
        description = "接取任务：接取任务 <任务>",
        aliases = "接受任务,接任务",
        category = "斗罗大陆·任务",
        scope = "all"
    )]
    fn accept_quest(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.accept_quest(req))
    }

    #[command(
        name = "任务进度",
        description = "查看进行中的任务进度：任务进度 [任务]",
        aliases = "我的任务,进行中任务",
        category = "斗罗大陆·任务",
        scope = "all"
    )]
    fn quest_progress(req: &CommandRequest) -> CommandResponse {
        with_service(req, true, true, |service| service.quest_progress(req))
    }

    #[command(
        name = "提交任务",
        description = "提交已完成任务并领取奖励：提交任务 <任务>",
        aliases = "完成任务,交任务",
        category = "斗罗大陆·任务",
        scope = "all"
    )]
    fn submit_quest(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.submit_quest(req))
    }

    #[command(
        name = "放弃任务",
        description = "放弃进行中的任务：放弃任务 <任务>",
        aliases = "取消任务",
        category = "斗罗大陆·任务",
        scope = "all"
    )]
    fn abandon_quest(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.abandon_quest(req))
    }

    #[command(
        name = "魂兽",
        description = "查看当前地图可挑战的魂兽：魂兽 [页码]",
        aliases = "魂兽列表,当前魂兽",
        category = "斗罗大陆·战斗",
        scope = "all"
    )]
    fn soul_beasts(req: &CommandRequest) -> CommandResponse {
        with_service(req, true, true, |service| service.soul_beasts(req))
    }

    #[command(
        name = "挑战",
        description = "挑战当前地图魂兽：挑战 <魂兽>",
        aliases = "挑战魂兽",
        category = "斗罗大陆·战斗",
        scope = "all"
    )]
    fn challenge(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.challenge(req))
    }

    #[command(
        name = "攻击",
        description = "进行一次普通攻击并承受魂兽反击",
        aliases = "打",
        category = "斗罗大陆·战斗",
        scope = "all"
    )]
    fn attack(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.attack(req))
    }

    #[command(
        name = "逃跑",
        description = "尝试结束当前魂兽战斗",
        aliases = "撤退",
        category = "斗罗大陆·战斗",
        scope = "all"
    )]
    fn flee(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.flee(req))
    }

    #[command(
        name = "战斗状态",
        description = "查看当前战斗快照",
        aliases = "战斗,查看战斗",
        category = "斗罗大陆·战斗",
        scope = "all"
    )]
    fn battle_status(req: &CommandRequest) -> CommandResponse {
        with_service(req, true, true, |service| service.battle_status(req))
    }

    #[command(
        name = "战斗日志",
        description = "查看最近战斗事件：战斗日志 [数量]",
        category = "斗罗大陆·战斗",
        scope = "all"
    )]
    fn battle_logs(req: &CommandRequest) -> CommandResponse {
        with_service(req, true, true, |service| service.battle_logs(req))
    }

    #[command(
        name = "旧档检查",
        description = "检查指定用户的旧版存档认领状态",
        category = "斗罗大陆·管理",
        role = "owner",
        scope = "private"
    )]
    fn inspect_legacy(req: &CommandRequest) -> CommandResponse {
        with_service(req, true, false, |service| service.inspect_legacy(req))
    }

    #[command(
        name = "旧档认领",
        description = "显式认领旧版存档：旧档认领 <用户ID> <当前account_id> 确认",
        category = "斗罗大陆·管理",
        role = "owner",
        scope = "private"
    )]
    fn claim_legacy(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, false, |service| service.claim_legacy(req))
    }

    #[command(
        name = "授权上下文",
        description = "授权群或频道：授权上下文 <group|channel> <上下文ID> [标签]",
        aliases = "新增授权,授权群",
        category = "斗罗大陆·管理",
        role = "owner",
        scope = "private"
    )]
    fn grant_context(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, false, |service| service.grant_context(req))
    }

    #[command(
        name = "取消授权",
        description = "撤销群或频道授权：取消授权 <group|channel> <上下文ID> 确认",
        aliases = "撤销授权,删除授权",
        category = "斗罗大陆·管理",
        role = "owner",
        scope = "private"
    )]
    fn revoke_context(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, false, |service| service.revoke_context(req))
    }

    #[command(
        name = "查看授权",
        description = "查看当前 Bot 的授权上下文列表",
        aliases = "授权列表",
        category = "斗罗大陆·管理",
        role = "owner",
        scope = "private"
    )]
    fn list_contexts(req: &CommandRequest) -> CommandResponse {
        with_service(req, true, false, |service| service.list_contexts(req))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use abi_stable::std_types::RString;

    use super::*;
    use crate::config::{AuthorizationMode, PluginConfig};
    use crate::store::IdentityKey;

    #[test]
    fn denied_allowlist_request_never_executes_economic_operation() {
        let directory = tempfile::tempdir().expect("应创建临时目录");
        let store = Store::initialize(directory.path(), &crate::config::DatabaseConfig::default())
            .expect("应初始化数据库");
        let key = IdentityKey {
            protocol: crate::message::Protocol::OneBot11,
            account_id: "10001",
            namespace: "default",
            subject_kind: "user",
            subject_id: "denied-user",
        };
        store
            .register_player(&key, "拒绝测试", "男")
            .expect("应创建测试角色");
        let mut config = PluginConfig::default();
        config.authorization.mode = AuthorizationMode::Allowlist;
        let service =
            GameService::with_assets(store.clone(), config, IllustrationAssets::default());
        let request = CommandRequest {
            args: RString::new(),
            command_name: RString::from("签到"),
            sender_id: RString::from("denied-user"),
            group_id: RString::from("unauthorized-group"),
            raw_event_json: RString::from(
                r#"{"self_id":"10001","qimen_context":{"version":1,"protocol":"onebot11","account_id":"10001"}}"#,
            ),
            sender_nickname: RString::new(),
            message_id: RString::from("denied-message"),
            timestamp: 0,
        };
        let executed = Cell::new(false);
        let (result, denied) = execute_service_operation(&service, &request, true, |service| {
            executed.set(true);
            service.daily_checkin(&request)
        });
        assert!(denied);
        assert!(result.is_err());
        assert!(!executed.get());
        assert_eq!(
            store
                .player_status(&key)
                .expect("应读取角色")
                .expect("角色应存在")
                .exp,
            0
        );
        assert!(
            store
                .list_operation_logs(&key, None, 100)
                .expect("应读取日志")
                .entries
                .iter()
                .all(|entry| entry.command != "签到")
        );
    }
}
