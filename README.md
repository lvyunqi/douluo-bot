# douluo-bot

QimenBot 动态插件版斗罗大陆文字游戏，插件 ID 为 `douluo-game`，面向 QimenBot 动态插件 API `0.6` 开发。

## Features

- 文本 RPG 基础流程：注册、武魂觉醒、地图、任务、经济、背包、PVE 战斗、魂环与魂技。
- 内容包扩展：武魂、魂技、效果、魂兽和魂环可通过 JSON/TOML 内容包发布为版本化目录数据。
- 跨协议消息：兼容 OneBot 11 与 QQ 官方机器人，回复优先使用通用消息段。
- 插图支持：支持本地 Base64 图片和公网 HTTPS 图片地址；未配置图片时仍返回完整文本。
- 可选媒体服务：仓库包含一个独立的静态图片服务示例，可用于向 QQ 官方 Markdown 暴露公网图片。

## Requirements

- QimenBot `0.1.18+`，推荐 `0.1.20+`。
- Rust stable，项目使用 `Cargo.lock` 固定依赖。
- Windows x64 MSVC、Linux x64 GNU 或 Linux ARM64 GNU。
- Linux musl 宿主不支持动态插件加载。

## Build

构建动态插件：

```powershell
cargo build --release --locked
```

平台产物：

- Windows：`target/release/qimen_dynamic_plugin_douluo_game.dll`
- Linux：`target/release/libqimen_dynamic_plugin_douluo_game.so`

可选构建静态图片服务：

```powershell
cargo build --manifest-path services/douluo-media/Cargo.toml --release --locked
```

## Load In QimenBot

1. 将当前平台的动态库复制到 QimenBot `plugin_bin_dir`。
2. 在 QimenBot Web 插件页重新扫描动态插件。
3. 启用 `douluo-game`。
4. 按需在 `config/plugins/douluo-game.toml` 中配置数据库、授权上下文、内容包和插图。

## Minimal Config

```toml
[database]
relative_path = "douluo-game/douluo.db"
busy_timeout_ms = 3000

[content]
package_file = ""
auto_publish = false

[illustrations]
enabled = false
mode = "direct"
direct_asset_root = "douluo-game/assets"
remote_base_url = ""
```

说明：

- `content.package_file` 必须是插件 `data_dir` 内的安全相对路径，支持 `.json` 和 `.toml`。
- `illustrations.mode = "direct"` 会读取本地图片并交给宿主按协议发送。
- `illustrations.mode = "remote"` 会拼接 `remote_base_url` 生成公网图片地址，适合 QQ 官方 Markdown。
- QQ 官方机器人使用远程图片时，地址必须是平台可访问的 HTTPS URL。

## Common Commands

插件会在 QimenBot 命令系统中注册游戏命令。常用入口包括：

- `斗罗系统`：查看游戏菜单。
- `开始穿越 <角色名> <男|女>`：创建角色。
- `武魂觉醒`：觉醒武魂。
- `状态`、`位置`、`背包`、`任务`、`魂兽`、`技能`、`魂环`：查看主要玩法状态。
- `挑战 <魂兽>`、`攻击`、`释放技能 <魂技>`、`逃跑`：进行 PVE 战斗。

命令前缀、群聊 @、私聊裸命令和管理员入口由 QimenBot 宿主统一配置。

## Development Checks

```powershell
cargo fmt --all -- --check
cargo check --locked --offline
cargo test --locked --offline --lib
cargo clippy --locked --offline --all-targets -- -D warnings
```

## License

[Apache License 2.0](LICENSE)
