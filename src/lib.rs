//! QimenBot 动态插件入口。

mod alias;
mod assets;
mod catalog;
pub mod config;
mod content;
mod context;
mod direct_asset_upload;
mod embedded_web;
mod game;
mod identity;
pub mod message;
pub mod player_stage_confirmation;
pub mod player_staging;
pub mod public_seed;
mod qq_media;
mod store;
mod web;

use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use abi_stable_host_api::{
    ACTION_REPLY, BotApi, CommandRequest, CommandResponse, DynamicActionResponse,
    InterceptorRequest, InterceptorResponse, PluginConfigRequest, PluginConfigResult,
    PluginInitConfig, PluginInitResult, SendBuilder,
};
use qimen_dynamic_plugin_derive::dynamic_plugin;
use serde_json::{Value, json};

use crate::assets::IllustrationAssets;
use crate::config::parse_config;
use crate::game::GameService;
use crate::message::{GameDocument, response_for};
use crate::store::Store;
use crate::web::ManagementServer;

static RUNTIME: OnceLock<RwLock<Option<Arc<GameService>>>> = OnceLock::new();
static MANAGEMENT_SERVER: OnceLock<Mutex<Option<ManagementServer>>> = OnceLock::new();
static RUNTIME_TRANSITION: OnceLock<Mutex<()>> = OnceLock::new();

fn runtime_slot() -> &'static RwLock<Option<Arc<GameService>>> {
    RUNTIME.get_or_init(|| RwLock::new(None))
}

fn management_server_slot() -> &'static Mutex<Option<ManagementServer>> {
    MANAGEMENT_SERVER.get_or_init(|| Mutex::new(None))
}

fn runtime_transition_slot() -> &'static Mutex<()> {
    RUNTIME_TRANSITION.get_or_init(|| Mutex::new(()))
}

fn initialize(config: PluginInitConfig) -> Result<(), String> {
    let _transition = runtime_transition_slot()
        .lock()
        .map_err(|_| "插件运行时切换锁已损坏".to_string())?;
    qq_media::clear_pending_images();
    let parsed = parse_config(config.config_json.as_str())?;
    catalog::validate_embedded_manifest()?;
    let data_dir = config.data_dir.as_str().trim();
    if data_dir.is_empty() {
        return Err("QimenBot 未提供 data_dir，无法安全创建游戏数据库".to_string());
    }
    let data_dir = Path::new(data_dir);
    let store = Store::initialize(data_dir, &parsed.database)?;
    if !parsed.content.package_file.is_empty() {
        let loaded = content::load_package_file(data_dir, &parsed.content.package_file)?;
        store.stage_content_package(&loaded)?;
        let report =
            store.validate_content_draft(&loaded.package.package_key, loaded.package.revision)?;
        if !report.errors.is_empty() {
            return Err(format!("内容包校验失败：{}", report.errors.join("；")));
        }
        if parsed.content.auto_publish {
            store.publish_content_draft(&loaded.package.package_key, loaded.package.revision)?;
        }
    }
    let web_config = parsed.web.clone();
    let illustration_config = parsed.illustrations.clone();
    let illustration_assets = IllustrationAssets::load(data_dir, &illustration_config)?;
    let service = Arc::new(GameService::with_assets(
        store.clone(),
        parsed,
        illustration_assets,
    ));
    replace_management_server(&web_config, &illustration_config, store, data_dir)?;
    let mut slot = runtime_slot()
        .write()
        .map_err(|_| "插件运行时锁已损坏".to_string())?;
    *slot = Some(service);
    Ok(())
}

/// 先释放旧监听再启动新配置；新配置失败时尽力恢复旧服务，避免 reload 留下空窗。
fn replace_management_server(
    web_config: &crate::config::WebConfig,
    illustration_config: &crate::config::IllustrationConfig,
    store: Store,
    data_dir: &Path,
) -> Result<(), String> {
    let mut slot = management_server_slot()
        .lock()
        .map_err(|_| "管理服务锁已损坏".to_string())?;
    let mut previous = slot.take();
    if let Some(server) = previous.as_mut() {
        server.stop()?;
    }
    match ManagementServer::start_if_enabled(web_config, illustration_config, store, data_dir) {
        Ok(replacement) => {
            *slot = replacement;
            Ok(())
        }
        Err(error) => {
            if let Some(server) = previous.as_mut()
                && let Err(restart_error) = server.restart()
            {
                return Err(format!(
                    "管理服务新配置启动失败：{error}；恢复旧服务失败：{restart_error}"
                ));
            }
            *slot = previous;
            Err(error)
        }
    }
}

fn shutdown_runtime() {
    let Ok(_transition) = runtime_transition_slot().lock() else {
        return;
    };
    qq_media::clear_pending_images();
    if let Ok(mut slot) = management_server_slot().lock()
        && let Some(mut server) = slot.take()
    {
        let _ = server.stop();
    }
    if let Ok(mut slot) = runtime_slot().write() {
        *slot = None;
    }
}

/// 动态拦截器只接收原始消息文本，先拆出无空白的快捷键和其余参数。
fn parse_player_alias_message(message_text: &str) -> Option<(&str, &str)> {
    let message_text = message_text.trim();
    if message_text.is_empty() || message_text.chars().any(char::is_control) {
        return None;
    }
    let (alias, args) = message_text
        .split_once(char::is_whitespace)
        .map_or((message_text, ""), |(alias, args)| (alias, args.trim()));
    (!alias.is_empty()).then_some((alias, args))
}

/// 将拦截器事件转换为现有游戏命令请求，保留身份、会话和幂等所需的宿主上下文。
fn command_request_from_interceptor(
    request: &InterceptorRequest,
    command_name: &str,
    args: &str,
) -> CommandRequest {
    CommandRequest {
        args: abi_stable::std_types::RString::from(args),
        command_name: abi_stable::std_types::RString::from(command_name),
        sender_id: request.sender_id.clone(),
        group_id: request.group_id.clone(),
        raw_event_json: request.raw_event_json.clone(),
        sender_nickname: request.sender_nickname.clone(),
        message_id: request.message_id.clone(),
        timestamp: request.timestamp,
    }
}

/// 查询当前玩家的快捷键，并构造用于执行 canonical 游戏命令的请求。
fn resolve_player_alias_interceptor(request: &InterceptorRequest) -> Option<CommandRequest> {
    let (alias, args) = parse_player_alias_message(request.message_text.as_str())?;
    let lookup_request = command_request_from_interceptor(request, alias, args);
    let service = runtime_slot().read().ok()?.clone()?;
    let command_name = service.resolve_player_alias_command(&lookup_request)?;
    Some(command_request_from_interceptor(
        request,
        &command_name,
        args,
    ))
}

enum InterceptorReplyTarget {
    Group(String),
    Private(String),
    Channel(String),
    ChannelPrivate(String),
}

/// 解析拦截器回复的目标，不依赖发送者可伪造的文本字段。
fn interceptor_reply_target(request: &InterceptorRequest) -> Option<InterceptorReplyTarget> {
    if !crate::message::detect_protocol(request.raw_event_json.as_str())
        .eq(&crate::message::Protocol::QqOfficial)
    {
        return (!request.group_id.is_empty())
            .then(|| InterceptorReplyTarget::Group(request.group_id.to_string()))
            .or_else(|| {
                (!request.sender_id.is_empty())
                    .then(|| InterceptorReplyTarget::Private(request.sender_id.to_string()))
            });
    }

    let raw_event: Value = serde_json::from_str(request.raw_event_json.as_str()).ok()?;
    let root = raw_event.as_object()?;
    let payload = root.get("qqbot_payload")?.as_object()?;
    let event_type = root
        .get("event_type")
        .and_then(Value::as_str)
        .or_else(|| payload.get("event_type").and_then(Value::as_str))?;
    let field = |name: &str| {
        root.get(name)
            .and_then(Value::as_str)
            .or_else(|| payload.get(name).and_then(Value::as_str))
    };

    match event_type {
        "GROUP_AT_MESSAGE_CREATE" | "GROUP_MESSAGE_CREATE" => field("group_openid")
            .filter(|value| !value.is_empty())
            .map(|value| InterceptorReplyTarget::Group(value.to_string())),
        "C2C_MESSAGE_CREATE" => (!request.sender_id.is_empty())
            .then(|| InterceptorReplyTarget::Private(request.sender_id.to_string())),
        "AT_MESSAGE_CREATE" | "MESSAGE_CREATE" => field("channel_id")
            .filter(|value| !value.is_empty())
            .map(|value| InterceptorReplyTarget::Channel(value.to_string())),
        "DIRECT_MESSAGE_CREATE" => field("guild_id")
            .filter(|value| !value.is_empty())
            .map(|value| InterceptorReplyTarget::ChannelPrivate(value.to_string())),
        _ => None,
    }
}

/// 在插件拦截器中把正常命令回执写入宿主的回调发送队列。
///
/// OneBot 与 QQ 群/C2C 可保留原富文本；频道/DMS 的主动发送仅安全降级为文本。
fn queue_interceptor_response(request: &InterceptorRequest, response: &CommandResponse) -> bool {
    if response.action.action_kind != ACTION_REPLY {
        return true;
    }
    let Some(target) = interceptor_reply_target(request) else {
        return false;
    };
    let is_onebot = crate::message::detect_protocol(request.raw_event_json.as_str())
        == crate::message::Protocol::OneBot11;
    let Some(segments_json) = interceptor_response_segments(&response.action, request, is_onebot)
    else {
        return false;
    };

    match target {
        InterceptorReplyTarget::Group(group_id) => {
            BotApi::send_group_rich(&group_id, &segments_json)
        }
        InterceptorReplyTarget::Private(user_id) => {
            BotApi::send_private_rich(&user_id, &segments_json)
        }
        InterceptorReplyTarget::Channel(channel_id) => {
            let Some(text) = interceptor_response_text(&response.action) else {
                return false;
            };
            SendBuilder::channel(&channel_id).text(&text).send();
        }
        InterceptorReplyTarget::ChannelPrivate(guild_id) => {
            let Some(text) = interceptor_response_text(&response.action) else {
                return false;
            };
            SendBuilder::channel_private(&guild_id).text(&text).send();
        }
    }
    true
}

/// 构造可由宿主发送队列消费的消息段；OneBot 额外保留对原消息的引用。
fn interceptor_response_segments(
    action: &DynamicActionResponse,
    request: &InterceptorRequest,
    include_reply: bool,
) -> Option<String> {
    let mut segments = if action.segments_json.is_empty() {
        vec![json!({"type":"text","data":{"text":action.message.as_str()}})]
    } else {
        serde_json::from_str::<Vec<Value>>(action.segments_json.as_str()).ok()?
    };
    if include_reply && !request.message_id.is_empty() {
        segments.insert(
            0,
            json!({"type":"reply","data":{"id":request.message_id.as_str()}}),
        );
    }
    serde_json::to_string(&segments).ok()
}

/// 从富文本回执提取频道/DMS 主动发送可承载的文本降级内容。
fn interceptor_response_text(action: &DynamicActionResponse) -> Option<String> {
    if !action.message.is_empty() {
        return Some(action.message.to_string());
    }
    let segments = serde_json::from_str::<Vec<Value>>(action.segments_json.as_str()).ok()?;
    let text = segments
        .iter()
        .filter_map(|segment| {
            let data = segment.get("data")?.as_object()?;
            match segment.get("type")?.as_str()? {
                "text" => data.get("text")?.as_str(),
                "markdown" => data.get("content")?.as_str(),
                _ => None,
            }
        })
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
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
    if outcome == "ok" {
        qq_media::stage_command_image(req, &document, service.illustration_config());
    }
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

/// 执行已由快捷键解析器限制过的游戏主命令，保持正式命令的授权、审计和事务语义。
fn dispatch_player_alias_command(request: &CommandRequest) -> CommandResponse {
    match request.command_name.as_str() {
        "斗罗系统" => with_service(request, true, true, |service| {
            service.menu(request.args.as_str())
        }),
        "开始穿越" => with_service(request, false, true, |service| service.register(request)),
        "武魂觉醒" => with_service(request, false, true, |service| service.awaken(request)),
        "开武魂" => with_service(request, false, true, |service| service.open_wuhun(request)),
        "关武魂" => with_service(request, false, true, |service| service.close_wuhun(request)),
        "放弃复活" => with_service(request, false, true, |service| {
            service.abandon_revival(request)
        }),
        "技能" => with_service(request, true, true, |service| service.skills(request)),
        "技能详情" => {
            with_service(request, true, true, |service| service.skill_detail(request))
        }
        "装备魂技" => {
            with_service(request, false, true, |service| service.equip_skill(request))
        }
        "卸下魂技" => with_service(request, false, true, |service| {
            service.unequip_skill(request)
        }),
        "魂环" => with_service(request, true, true, |service| service.soul_rings(request)),
        "吸收魂环" => with_service(request, false, true, |service| {
            service.absorb_soul_ring(request)
        }),
        "剥离魂环" => with_service(request, false, true, |service| {
            service.detach_soul_ring(request)
        }),
        "释放技能" => with_service(request, false, true, |service| service.use_skill(request)),
        "签到" => with_service(request, false, true, |service| {
            service.daily_checkin(request)
        }),
        "钱包" => with_service(request, true, true, |service| service.wallet(request)),
        "转账" => with_service(request, false, true, |service| {
            service.transfer_gold(request)
        }),
        "NPC" => with_service(request, true, true, |service| service.npcs(request)),
        "对话" => with_service(request, false, true, |service| service.talk(request)),
        "商店" => with_service(request, true, true, |service| service.shop(request)),
        "背包" => with_service(request, true, true, |service| service.inventory(request)),
        "储物器" => with_service(request, true, true, |service| {
            service.storage_containers(request)
        }),
        "查看储物器" => {
            with_service(request, true, true, |service| service.view_storage(request))
        }
        "存入" => with_service(request, false, true, |service| service.store_item(request)),
        "取出" => with_service(request, false, true, |service| {
            service.withdraw_item(request)
        }),
        "封印储物器" => with_service(request, false, true, |service| {
            service.seal_storage(request)
        }),
        "解封储物器" => with_service(request, false, true, |service| {
            service.unseal_storage(request)
        }),
        "装备魂导器" => with_service(request, false, true, |service| {
            service.equip_storage(request)
        }),
        "卸下魂导器" => with_service(request, false, true, |service| {
            service.unequip_storage(request)
        }),
        "购买" => with_service(request, false, true, |service| service.buy(request)),
        "出售" => with_service(request, false, true, |service| service.sell(request)),
        "使用" => with_service(request, false, true, |service| service.use_item(request)),
        "发送物品" => with_service(request, false, true, |service| service.gift_item(request)),
        "状态" => with_service(request, true, true, |service| service.status(request)),
        "排行榜" => with_service(request, false, true, |service| service.leaderboard(request)),
        "位置" => with_service(request, true, true, |service| service.location(request)),
        "地图列表" => with_service(request, true, true, |service| service.map_list(request)),
        "数值曲线" => with_service(request, true, true, |service| {
            service.numeric_curves(request)
        }),
        "向" => with_service(request, false, true, |service| {
            service.move_direction(request)
        }),
        "传送" => with_service(request, false, true, |service| service.teleport(request)),
        "掉落" => with_service(request, true, true, |service| service.ground_drops(request)),
        "拾取" => with_service(request, false, true, |service| {
            service.pick_up_ground_drop(request)
        }),
        "任务" => with_service(request, true, true, |service| service.quests(request)),
        "接取任务" => with_service(request, false, true, |service| {
            service.accept_quest(request)
        }),
        "任务进度" => with_service(request, true, true, |service| {
            service.quest_progress(request)
        }),
        "提交任务" => with_service(request, false, true, |service| {
            service.submit_quest(request)
        }),
        "放弃任务" => with_service(request, false, true, |service| {
            service.abandon_quest(request)
        }),
        "魂兽" => with_service(request, true, true, |service| service.soul_beasts(request)),
        "挑战" => with_service(request, false, true, |service| service.challenge(request)),
        "攻击" => with_service(request, false, true, |service| service.attack(request)),
        "逃跑" => with_service(request, false, true, |service| service.flee(request)),
        "战斗状态" => with_service(request, true, true, |service| {
            service.battle_status(request)
        }),
        "战斗日志" => with_service(request, true, true, |service| service.battle_logs(request)),
        // 解析层已经验证目标集合；意外值保持忽略，避免自行扩大命令权限。
        _ => CommandResponse::ignore(),
    }
}

#[dynamic_plugin(
    id = "douluo-game",
    version = "0.1.3",
    api = "0.6",
    config_schema = "../config.schema.json",
    config_ui = "../config.ui.json",
    config_version = 7,
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

    /// 在宿主命令匹配前处理玩家快捷键；未命中或无法安全回复时放行原消息。
    #[pre_handle]
    fn player_alias_interceptor(request: &InterceptorRequest) -> InterceptorResponse {
        let Some(canonical_request) = resolve_player_alias_interceptor(request) else {
            return InterceptorResponse::allow();
        };
        let response = dispatch_player_alias_command(&canonical_request);
        if queue_interceptor_response(request, &response) {
            if let Some(image) = qq_media::take_after_completion(request) {
                qq_media::send_image(image);
            }
            InterceptorResponse::block()
        } else {
            qq_media::discard_for_interceptor(request);
            InterceptorResponse::allow()
        }
    }

    /// 主回复成功后才发送 QQ 群/C2C 的独立本地图片，避免图片先于完整正文出现。
    #[after_completion]
    fn qq_official_inline_image_after_completion(request: &InterceptorRequest) {
        if let Some(image) = qq_media::take_after_completion(request) {
            qq_media::send_image(image);
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
        name = "放弃复活",
        description = "复活窗口结束后放弃当前生命并进入下一世",
        aliases = "放弃重生,重生",
        category = "斗罗大陆·角色",
        scope = "all"
    )]
    fn abandon_revival(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.abandon_revival(req))
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
        name = "装备魂技",
        description = "装备已学习魂技：装备魂技 <魂技>",
        aliases = "装备技能",
        category = "斗罗大陆·角色",
        scope = "all"
    )]
    fn equip_skill(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.equip_skill(req))
    }

    #[command(
        name = "卸下魂技",
        description = "卸下已装备魂技：卸下魂技 <魂技>",
        aliases = "卸下技能",
        category = "斗罗大陆·角色",
        scope = "all"
    )]
    fn unequip_skill(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.unequip_skill(req))
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
        name = "剥离魂环",
        description = "剥离当前最高环位的魂环及其绑定魂技",
        aliases = "剥离",
        category = "斗罗大陆·角色",
        scope = "all"
    )]
    fn detach_soul_ring(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.detach_soul_ring(req))
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
        name = "设置快捷键",
        description = "设置个人命令快捷键：设置快捷键 <原指令>-<新指令>",
        category = "斗罗大陆·角色",
        scope = "all"
    )]
    fn set_player_alias(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.set_player_alias(req))
    }

    #[command(
        name = "快捷键列表",
        description = "查看个人快捷键列表",
        category = "斗罗大陆·角色",
        scope = "all"
    )]
    fn list_player_aliases(req: &CommandRequest) -> CommandResponse {
        with_service(req, true, true, |service| service.list_player_aliases(req))
    }

    #[command(
        name = "查看快捷键",
        description = "查看原指令下的个人快捷键：查看快捷键 <原指令>",
        category = "斗罗大陆·角色",
        scope = "all"
    )]
    fn player_alias_detail(req: &CommandRequest) -> CommandResponse {
        with_service(req, true, true, |service| service.player_alias_detail(req))
    }

    #[command(
        name = "删除快捷键",
        description = "删除个人快捷键：删除快捷键 <原指令>-<快捷键>",
        category = "斗罗大陆·角色",
        scope = "all"
    )]
    fn delete_player_alias(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.delete_player_alias(req))
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
        name = "储物器",
        description = "查看已绑定储物器：储物器",
        aliases = "储物,储物器列表",
        category = "斗罗大陆·经济",
        scope = "all"
    )]
    fn storage_containers(req: &CommandRequest) -> CommandResponse {
        with_service(req, true, true, |service| service.storage_containers(req))
    }

    #[command(
        name = "查看储物器",
        description = "查看未封印储物器内容：查看储物器 <储物器>",
        aliases = "打开储物器",
        category = "斗罗大陆·经济",
        scope = "all"
    )]
    fn view_storage(req: &CommandRequest) -> CommandResponse {
        with_service(req, true, true, |service| service.view_storage(req))
    }

    #[command(
        name = "存入",
        description = "把随身物品存入储物器：存入 <储物器> <物品> [数量]",
        category = "斗罗大陆·经济",
        scope = "all"
    )]
    fn store_item(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.store_item(req))
    }

    #[command(
        name = "取出",
        description = "从储物器取回物品：取出 <储物器> <物品> [数量]",
        category = "斗罗大陆·经济",
        scope = "all"
    )]
    fn withdraw_item(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.withdraw_item(req))
    }

    #[command(
        name = "封印储物器",
        description = "封印当前玩家绑定的储物器：封印储物器 <储物器>",
        category = "斗罗大陆·经济",
        scope = "all"
    )]
    fn seal_storage(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.seal_storage(req))
    }

    #[command(
        name = "解封储物器",
        description = "解封并生成一次性随机属性：解封储物器 <储物器>",
        aliases = "解封",
        category = "斗罗大陆·经济",
        scope = "all"
    )]
    fn unseal_storage(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.unseal_storage(req))
    }

    #[command(
        name = "装备魂导器",
        description = "装备已解封的便携魂导器：装备魂导器 <储物器>",
        aliases = "装备储物器",
        category = "斗罗大陆·经济",
        scope = "all"
    )]
    fn equip_storage(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.equip_storage(req))
    }

    #[command(
        name = "卸下魂导器",
        description = "卸下便携魂导器：卸下魂导器 <储物器>",
        aliases = "卸下储物器",
        category = "斗罗大陆·经济",
        scope = "all"
    )]
    fn unequip_storage(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.unequip_storage(req))
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
        name = "排行榜",
        description = "分页查看同一机器人分区内的基础等级排行：排行榜 [页码]",
        aliases = "排行,排名",
        category = "斗罗大陆·角色",
        scope = "all"
    )]
    fn leaderboard(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.leaderboard(req))
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
        name = "数值曲线",
        description = "分页查看当前发布的等级与魂技成长规则说明：数值曲线 [页码]",
        aliases = "成长曲线,曲线列表",
        category = "斗罗大陆·角色",
        scope = "all"
    )]
    fn numeric_curves(req: &CommandRequest) -> CommandResponse {
        with_service(req, true, true, |service| service.numeric_curves(req))
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
        name = "决斗",
        description = "向同一身份域的玩家发起决斗邀请：决斗 <用户ID>",
        category = "斗罗大陆·战斗",
        scope = "all"
    )]
    fn duel(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.duel(req))
    }

    #[command(
        name = "决斗状态",
        description = "查看当前身份发出和收到的未过期决斗邀请",
        category = "斗罗大陆·战斗",
        scope = "all"
    )]
    fn duel_status(req: &CommandRequest) -> CommandResponse {
        with_service(req, true, true, |service| service.duel_status(req))
    }

    #[command(
        name = "接受决斗",
        description = "接受指定挑战者的决斗邀请：接受决斗 <挑战者ID>",
        category = "斗罗大陆·战斗",
        scope = "all"
    )]
    fn accept_duel(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.accept_duel(req))
    }

    #[command(
        name = "取消决斗",
        description = "取消自己发出的决斗邀请：取消决斗 <目标ID>",
        category = "斗罗大陆·战斗",
        scope = "all"
    )]
    fn cancel_duel(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, true, |service| service.cancel_duel(req))
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
    use std::net::TcpListener;
    use std::sync::{Mutex, OnceLock};

    use abi_stable::std_types::RString;

    use super::*;
    use crate::config::{AuthorizationMode, IllustrationConfig, PluginConfig, WebConfig};
    use crate::store::IdentityKey;

    static MANAGEMENT_RUNTIME_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn management_runtime_test_lock() -> std::sync::MutexGuard<'static, ()> {
        MANAGEMENT_RUNTIME_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("管理运行时测试锁应可用")
    }

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

    #[test]
    fn management_server_reload_failure_restores_the_previous_listener() {
        let _guard = management_runtime_test_lock();
        let directory = tempfile::tempdir().expect("应创建管理服务临时目录");
        let store = Store::initialize(directory.path(), &crate::config::DatabaseConfig::default())
            .expect("应初始化管理服务数据库");
        let enabled = WebConfig {
            enabled: true,
            port: 0,
            admin_secret: "0123456789abcdef".to_string(),
            ..WebConfig::default()
        };
        let illustrations = IllustrationConfig::default();
        replace_management_server(&enabled, &illustrations, store.clone(), directory.path())
            .expect("应启动初始管理服务");
        assert!(
            management_server_slot()
                .lock()
                .expect("管理服务锁应可用")
                .is_some()
        );

        let invalid = WebConfig {
            bind: "invalid-bind".to_string(),
            ..enabled
        };
        assert!(
            replace_management_server(&invalid, &illustrations, store.clone(), directory.path())
                .is_err()
        );
        assert!(
            management_server_slot()
                .lock()
                .expect("恢复后的管理服务锁应可用")
                .is_some()
        );

        let disabled = WebConfig::default();
        replace_management_server(&disabled, &illustrations, store, directory.path())
            .expect("禁用配置应停止管理服务");
        assert!(
            management_server_slot()
                .lock()
                .expect("停止后的管理服务锁应可用")
                .is_none()
        );
    }

    #[test]
    fn plugin_initialize_and_shutdown_own_the_management_server_lifecycle() {
        let _guard = management_runtime_test_lock();
        shutdown_runtime();
        let directory = tempfile::tempdir().expect("应创建插件临时数据目录");
        let port = TcpListener::bind("127.0.0.1:0")
            .expect("应预留回环端口")
            .local_addr()
            .expect("应读取预留端口")
            .port();
        let config_json = serde_json::json!({
            "web": {
                "enabled": true,
                "bind": "127.0.0.1",
                "port": port,
                "admin_secret": "0123456789abcdef"
            }
        })
        .to_string();
        initialize(PluginInitConfig {
            plugin_id: RString::from("douluo-game"),
            config_json: RString::from(config_json),
            plugin_dir: RString::new(),
            data_dir: RString::from(directory.path().to_string_lossy().to_string()),
        })
        .expect("插件初始化应启动管理服务");
        assert!(runtime_slot().read().expect("插件运行时锁应可用").is_some());
        assert!(
            management_server_slot()
                .lock()
                .expect("管理服务锁应可用")
                .is_some()
        );

        shutdown_runtime();
        assert!(
            runtime_slot()
                .read()
                .expect("关闭后的插件运行时锁应可用")
                .is_none()
        );
        assert!(
            management_server_slot()
                .lock()
                .expect("关闭后的管理服务锁应可用")
                .is_none()
        );
    }
}
