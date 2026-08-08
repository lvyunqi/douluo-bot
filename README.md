# douluo-bot

QimenBot 动态插件版斗罗大陆文字游戏，插件 ID 为 `douluo-game`，面向 QimenBot 动态插件 API `0.6` 开发。

## Features

- 文本 RPG 基础流程：注册、武魂觉醒、地图、任务、经济、背包、PVE 战斗、魂环与魂技。
- 内容包扩展：武魂、魂技、效果、魂兽和魂环可通过 JSON/TOML 内容包发布为版本化目录数据。
- 跨协议消息：兼容 OneBot 11 与 QQ 官方机器人，回复优先使用通用消息段。
- 插图支持：支持本地 Base64 图片和公网 HTTPS 图片地址；未配置图片时仍返回完整文本。
- 可选媒体服务：仓库包含一个独立的静态图片服务示例，可用于向 QQ 官方 Markdown 暴露公网图片。
- 可选管理服务：默认仅回环监听，提供健康检查、短期管理会话、内容元数据、已暂存草稿的校验/发布与追加式管理员审计。

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
4. 按需在 `config/plugins/douluo-game.toml` 中配置数据库、授权上下文、内容包、管理服务和插图。

## Minimal Config

```toml
[database]
relative_path = "douluo-game/douluo.db"
busy_timeout_ms = 3000

[content]
package_file = ""
auto_publish = false

[web]
enabled = false
bind = "127.0.0.1"
port = 18181
allow_remote = false
public_base_url = ""
# 启用管理服务时必须配置 16-256 字符的 admin_secret。

[illustrations]
enabled = false
mode = "direct"
direct_asset_root = "douluo-game/assets"
remote_base_url = ""
```

说明：

- `content.package_file` 必须是插件 `data_dir` 内的安全相对路径，支持 `.json` 和 `.toml`。
- `web.enabled` 默认关闭；启用时 `web.bind` 只能是 IP 地址，默认仅允许 `127.0.0.1`/`::1` 等回环监听。
- 非回环监听必须同时设置 `web.allow_remote = true` 和公网 HTTPS `web.public_base_url`，并由反向代理终止 TLS。
- `web.admin_secret` 只用于建立短期 HttpOnly 会话，不会由插件显示或写入日志。
- `illustrations.mode = "direct"` 会读取本地图片并交给宿主按协议发送。
- `illustrations.mode = "remote"` 会拼接 `remote_base_url` 生成公网图片地址，适合 QQ 官方 Markdown。
- QQ 官方机器人使用远程图片时，地址必须是平台可访问的 HTTPS URL。

## Management API

启用管理服务后，`/healthz` 与 `/readyz` 可用于本机进程检查。管理 API 默认同源且不开放 CORS：

- `POST /api/v1/session`：以管理密钥建立短期会话并返回 CSRF token。
- `GET /api/v1/session`、`DELETE /api/v1/session`：读取或结束当前会话；退出请求需要 CSRF token。
- `GET /api/v1/content/active`：读取当前激活内容 revision，要求 `content_admin` 会话。
- `GET /api/v1/content/revisions`：读取 revision 元数据和成员数量，使用 `after_id` 游标与 `limit=1..100`。
- `GET /api/v1/content/drafts`：读取草稿状态、哈希和校验错误，使用相同游标；不返回草稿正文。
- `GET /api/v1/content/drafts/{package_key}/{package_revision}/diff`：读取草稿相对当前 active revision 的成员差异；不返回正文且不改变草稿状态。
- `GET /api/v1/content/activations`：读取追加式 activation 历史，使用相同游标。
- `POST /api/v1/content/drafts/{package_key}/{package_revision}/validate`：校验已暂存草稿；要求 `content_admin` 会话和 `X-CSRF-Token`，不接收草稿正文。
- `POST /api/v1/content/drafts/{package_key}/{package_revision}/publish`：发布已校验草稿；首次发布返回 `201`，重放返回 `200`，同样要求 CSRF。
- `GET /api/v1/content/operations`：读取追加式管理员操作审计，使用相同游标；不会返回会话指纹。

写路由只操作已经由受控文件入口暂存的草稿，并通过 Store 事务同步写入管理员审计；不提供草稿正文、内容上传、目录直写或 rollback 接口，也不会迁移或改写玩家、魂环或魂技状态。

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
