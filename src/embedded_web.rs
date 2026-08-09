//! 内置管理端构建产物的编译期资源清单。

use rust_embed::RustEmbed;

/// 管理端仅在构建期读取 `web/dist`，运行时不依赖 Node.js 或文件系统中的前端产物。
#[derive(RustEmbed)]
#[folder = "web/dist/"]
pub(crate) struct ManagementWebAssets;
