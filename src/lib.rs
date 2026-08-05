//! QimenBot 动态插件入口。

mod assets;
mod catalog;
pub mod config;
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
    operation: impl FnOnce(&GameService) -> Result<GameDocument, String>,
) -> CommandResponse {
    let service = match runtime_slot().read() {
        Ok(slot) => slot.clone(),
        Err(_) => return CommandResponse::text("插件运行时锁已损坏，请联系管理员"),
    };
    let Some(service) = service else {
        return CommandResponse::text("斗罗大陆插件尚未完成初始化，请联系管理员");
    };
    let result = operation(&service);
    let outcome = if result.is_ok() { "ok" } else { "error" };
    // Successful mutations write their audit row in the same SQLite
    // transaction. Read-only commands and failed attempts are best-effort.
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

#[dynamic_plugin(
    id = "douluo-game",
    version = "0.1.0",
    api = "0.6",
    config_schema = "../config.schema.json",
    config_ui = "../config.ui.json",
    config_version = 3,
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
        with_service(req, true, |service| service.menu(req.args.as_str()))
    }

    #[command(
        name = "开始穿越",
        description = "创建斗罗大陆角色：开始穿越 <角色名> <男|女>",
        aliases = "开始转生",
        category = "斗罗大陆·角色",
        scope = "all"
    )]
    fn register(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, |service| service.register(req))
    }

    #[command(
        name = "武魂觉醒",
        description = "觉醒第一武魂",
        aliases = "觉醒",
        category = "斗罗大陆·角色",
        scope = "all"
    )]
    fn awaken(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, |service| service.awaken(req))
    }

    #[command(
        name = "状态",
        description = "查看自己的角色状态",
        aliases = "我的状态,属性",
        category = "斗罗大陆·角色",
        scope = "all"
    )]
    fn status(req: &CommandRequest) -> CommandResponse {
        with_service(req, true, |service| service.status(req))
    }

    #[command(
        name = "位置",
        description = "查看当前地图",
        aliases = "地图,当前位置",
        category = "斗罗大陆·世界",
        scope = "all"
    )]
    fn location(req: &CommandRequest) -> CommandResponse {
        with_service(req, true, |service| service.location(req))
    }

    #[command(
        name = "旧档检查",
        description = "检查指定用户的旧版存档认领状态",
        category = "斗罗大陆·管理",
        role = "owner",
        scope = "private"
    )]
    fn inspect_legacy(req: &CommandRequest) -> CommandResponse {
        with_service(req, true, |service| service.inspect_legacy(req))
    }

    #[command(
        name = "旧档认领",
        description = "显式认领旧版存档：旧档认领 <用户ID> <当前account_id> 确认",
        category = "斗罗大陆·管理",
        role = "owner",
        scope = "private"
    )]
    fn claim_legacy(req: &CommandRequest) -> CommandResponse {
        with_service(req, false, |service| service.claim_legacy(req))
    }
}
