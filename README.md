# douluo-bot

QimenBot 动态插件版斗罗大陆文字游戏，插件 ID 为 `douluo-game`，面向 QimenBot 动态插件 API `0.6` 开发。

## Features

- 文本 RPG 基础流程：注册、武魂觉醒、地图、任务、经济、背包、PVE 战斗、魂环与魂技。
- 内容包扩展：武魂、魂技、效果、魂兽和魂环可通过 JSON/TOML 内容包发布为版本化目录数据。
- 跨协议消息：兼容 OneBot 11 与 QQ 官方机器人，回复优先使用通用消息段。
- 插图支持：支持本地 Base64 图片和公网 HTTPS 图片地址；未配置图片时仍返回完整文本。
- 可选媒体服务：仓库包含一个独立的静态图片服务示例，可用于向 QQ 官方 Markdown 暴露公网图片。
- 可选管理服务：默认仅回环监听，提供健康检查、短期管理会话、内容元数据、受限内容文件暂存、草稿校验/发布/回滚与追加式管理员审计。
- 内置管理端：使用 React、Vite 和 shadcn/ui 构建，只读呈现内容 revision 与脱敏审计数据；构建产物编入动态插件，不依赖 QimenBot 宿主页。

## Requirements

- QimenBot `0.1.18+`，推荐 `0.1.20+`。
- Rust stable，项目使用 `Cargo.lock` 固定依赖。
- Windows x64 MSVC、Linux x64 GNU 或 Linux ARM64 GNU。
- Linux musl 宿主不支持动态插件加载。

## Build

构建动态插件前，构建机需要 Node.js、pnpm 和管理端依赖；它们只在编译期使用：

```powershell
pnpm --dir web install --frozen-lockfile
```

随后构建动态插件：

```powershell
cargo build --release --locked
```

平台产物：

- Windows：`target/release/qimen_dynamic_plugin_douluo_game.dll`
- Linux：`target/release/libqimen_dynamic_plugin_douluo_game.so`

`build.rs` 会执行 `pnpm --dir web run build`，再将 `web/dist` 的哈希 CSS、JavaScript 和本地字体通过 `rust-embed` 编入动态库。部署运行时不需要 Node.js、pnpm、`web/node_modules` 或 `web/dist`。

可选构建静态图片服务：

```powershell
cargo build --manifest-path services/douluo-media/Cargo.toml --release --locked
```

生产只读卷、Caddy HTTPS/base-path 配置、资源更新重启边界和插件 `remote` 模式冒烟见
[douluo-media 部署说明](deploy/douluo-media/README.md)。
媒体服务只读取本地 published root 和 SQLite catalog；`chat`、`thumb`、`large` 变体位于 `__variants/`，不使用对象存储。
切换只读卷前可执行纯离线校验，不会启动 HTTP 服务或写入 catalog：

```text
douluo-media catalog verify --root <published-root>
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
- `POST /api/v1/content/drafts/stage`：只接受 `{ "package_file": "安全相对路径" }`，从插件 `data_dir` 读取既有 UTF-8 `.json`/`.toml` 常规文件并暂存；要求 `content_admin` 会话和 `X-CSRF-Token`，不接收正文且不写文件系统。
- `GET /api/v1/content/drafts/{package_key}/{package_revision}/diff`：读取草稿相对当前 active revision 的成员差异；不返回正文且不改变草稿状态。
- `GET /api/v1/content/activations`：读取追加式 activation 历史，使用相同游标。
- `POST /api/v1/content/drafts/{package_key}/{package_revision}/validate`：校验已暂存草稿；要求 `content_admin` 会话和 `X-CSRF-Token`，不接收草稿正文。
- `POST /api/v1/content/drafts/{package_key}/{package_revision}/publish`：发布已校验草稿；首次发布返回 `201`，重放返回 `200`，同样要求 CSRF。
- `GET /api/v1/content/operations`：读取追加式管理员操作审计，使用相同游标；不会返回会话指纹。
- `POST /api/v1/content/revisions/{revision_id}/rollback`：回滚到已存在的正 revision；要求 `content_admin` 会话和 `X-CSRF-Token`，只追加一条 `rollback` activation，返回 `201`。
- `GET /api/v1/content/rollback-operations`：读取 rollback 专用的追加式管理员审计，使用相同游标；不会返回会话指纹。
- `GET /api/v1/content/stage-operations`：读取受限暂存的追加式管理员审计，使用相同游标；不会返回会话指纹或文件路径。

写路由只操作既有文件、已暂存草稿或既有 revision，并通过 Store 事务同步写入管理员审计；不提供草稿正文、目录直写或文件系统写入。暂存不会创建、覆盖或删除文件。rollback 不删除目录、草稿或 revision，不恢复已剥离魂环/魂技，也不会迁移或改写任何玩家、魂环或魂技状态。

## Management UI

启用 `[web]` 后，访问配置的管理端根地址（默认 `http://127.0.0.1:18181/`）即可使用内置页面。它由 shadcn/ui 的 Button、Input、Tabs、Table、Badge、Skeleton、Alert 和 Tooltip 组成，并且只请求以下既有接口：

- 会话建立、读取和退出。
- active revision、草稿、revision、activation 元数据。
- `operations`、`rollback-operations` 与 `stage-operations` 的脱敏游标分页。

页面不调用 stage、validate、publish 或 rollback，不提供文件、目录或玩家状态写控件。管理密钥仅用于登录请求；cookie 由 HttpOnly/SameSite 会话管理，CSRF token 只保留在页面内存。服务只提供 `/` 和精确的 `/assets/*` 静态资源路径，未知路径返回 404；页面 CSP 限制脚本、样式、连接和字体为同源，所有静态响应均使用 `no-store`、`nosniff`、`DENY` 和 `no-referrer`。

## Common Commands

插件会在 QimenBot 命令系统中注册游戏命令。常用入口包括：

- `斗罗系统`：查看游戏菜单。
- `开始穿越 <角色名> <男|女>`：创建角色。
- `武魂觉醒`：觉醒武魂。
- `状态`、`位置`、`背包`、`任务`、`魂兽`、`技能`、`魂环`：查看主要玩法状态。
- `挑战 <魂兽>`、`攻击`、`释放技能 <魂技>`、`逃跑`：进行 PVE 战斗。

命令前缀、群聊 @、私聊裸命令和管理员入口由 QimenBot 宿主统一配置。

## QQ Official Manual Smoke

`scripts/qq-official-smoke.ps1` prepares an isolated Windows QimenBot host for the QQ
official group/C2C `direct` image check. Its default mode only validates the DLL, host
binary, and whether `QQBOT_APPID` / `QQBOT_SECRET` are present; it does not start a
Gateway or make a network request.

After configuring those environment variables and enabling `GROUP_AND_C2C_EVENT` for a
dedicated test bot, start the manual run explicitly:

```powershell
.\scripts\qq-official-smoke.ps1 -HostWorktree C:\projects\QimenBot -StartGateway
```

The script uses a temporary 1x1 WebP and asks the operator to verify a group mention and
a C2C `斗罗系统` command. Each should display complete text before one independent image.
It does not print credentials, Base64, or raw message logs, and cleans up its temporary
host after the operator finishes.

## Development Checks

```powershell
cargo fmt --all -- --check
pnpm --dir web build
cargo check --locked --offline
cargo test --locked --offline --lib
cargo clippy --locked --offline --all-targets -- -D warnings
```

## License

[Apache License 2.0](LICENSE)
