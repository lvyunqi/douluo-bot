//! QimenBot 动态插件入口。

mod config;
mod game;
mod message;
mod store;

use std::path::Path;
use std::sync::{Arc, OnceLock, RwLock};

use abi_stable_host_api::{
    CommandRequest, CommandResponse, PluginConfigRequest, PluginConfigResult, PluginInitConfig,
    PluginInitResult,
};
use qimen_dynamic_plugin_derive::dynamic_plugin;

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
    let data_dir = config.data_dir.as_str().trim();
    if data_dir.is_empty() {
        return Err("QimenBot 未提供 data_dir，无法安全创建游戏数据库".to_string());
    }
    let store = Store::initialize(Path::new(data_dir), &parsed.database)?;
    let service = Arc::new(GameService::new(store, parsed));
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
    operation: impl FnOnce(&GameService) -> Result<GameDocument, String>,
) -> CommandResponse {
    let service = match runtime_slot().read() {
        Ok(slot) => slot.clone(),
        Err(_) => return CommandResponse::text("插件运行时锁已损坏，请联系管理员"),
    };
    let Some(service) = service else {
        return CommandResponse::text("斗罗大陆插件尚未完成初始化，请联系管理员");
    };
    let document = match operation(&service) {
        Ok(document) => document,
        Err(error) => GameDocument::new("操作失败").line(error),
    };
    response_for(req, &document, service.message_config())
}

#[dynamic_plugin(
    id = "douluo-game",
    version = "0.1.0",
    api = "0.6",
    config_schema = "../config.schema.json",
    config_ui = "../config.ui.json",
    config_version = 1,
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
        description = "查看斗罗大陆游戏主菜单",
        aliases = "斗罗菜单,菜单",
        category = "斗罗大陆"
    )]
    fn menu(req: &CommandRequest) -> CommandResponse {
        with_service(req, |service| Ok(service.menu()))
    }

    #[command(
        name = "开始穿越",
        description = "创建斗罗大陆角色",
        aliases = "开始转生",
        category = "斗罗大陆"
    )]
    fn register(req: &CommandRequest) -> CommandResponse {
        with_service(req, |service| service.register(req))
    }

    #[command(
        name = "武魂觉醒",
        description = "觉醒第一武魂",
        aliases = "觉醒",
        category = "斗罗大陆"
    )]
    fn awaken(req: &CommandRequest) -> CommandResponse {
        with_service(req, |service| service.awaken(req))
    }

    #[command(
        name = "状态",
        description = "查看自己的角色状态",
        aliases = "我的状态,属性",
        category = "斗罗大陆"
    )]
    fn status(req: &CommandRequest) -> CommandResponse {
        with_service(req, |service| service.status(req))
    }
}
