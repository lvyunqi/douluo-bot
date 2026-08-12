# douluo-bot

QimenBot 动态插件版斗罗大陆文字游戏，插件 ID 为 `douluo-game`，面向 QimenBot 动态插件 API `0.6` 开发。

## Features

- 文本 RPG 基础流程：注册、武魂觉醒、地图、任务、经济、背包、PVE 战斗、魂环与魂技。
- 内容包扩展：武魂、魂技、效果、魂兽和魂环可通过 JSON/TOML 内容包发布为版本化目录数据。
- 跨协议消息：兼容 OneBot 11 与 QQ 官方机器人，回复优先使用通用消息段。
- 插图支持：支持本地 Base64 图片和公网 HTTPS 图片地址；未配置图片时仍返回完整文本。
- 可选媒体服务：仓库包含一个独立的静态图片服务示例，可用于向 QQ 官方 Markdown 暴露公网图片。
- 可选管理服务：默认仅回环监听，提供健康检查、短期管理会话、内容元数据、JSON/TOML 内容包上传、草稿校验/发布/回滚、受限 direct 本地插图上传与追加式管理员审计。
- 内置管理端：使用 React、Vite 和 shadcn/ui 构建，呈现内容 revision、脱敏审计、内容包上传与受限本地插图上传；构建产物编入动态插件，不依赖 QimenBot 宿主页。

## Requirements

- QimenBot `0.1.18+`，推荐 `0.1.20+`。
- Rust stable，项目使用 `Cargo.lock` 固定依赖。
- Windows、GNU/Linux、macOS 的 x86_64 或 ARM64；Release 提供六个原生 target。
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

- Windows：`qimen_dynamic_plugin_douluo_game-{x86_64|aarch64}-pc-windows-msvc.dll`
- GNU/Linux：`libqimen_dynamic_plugin_douluo_game-{x86_64|aarch64}-unknown-linux-gnu.so`
- macOS：`libqimen_dynamic_plugin_douluo_game-{x86_64|aarch64}-apple-darwin.dylib`

GNU/Linux Release 在 Debian 11 构建，最低 glibc 为 `2.31`。发布资产、字节数和 SHA256 见
[v0.1.1 Release](https://github.com/lvyunqi/douluo-bot/releases/tag/v0.1.1)。

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
4. 按需在 QimenBot 插件配置页设置授权上下文、管理服务和插图；首次保存会创建 `config/plugins/douluo-game.toml`。

## Minimal Config

```toml
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

- SQLite 固定默认位于插件 `data_dir/douluo-game/douluo.db`，等待超时固定为 3000 毫秒；在线表单不再暴露这两个内部存储字段。旧配置文件中的 `[database]` 仍兼容读取，避免现有部署切换数据库。
- 新内容包在斗罗管理端直接选择 UTF-8 `.json`/`.toml` 文件上传，不需要先复制到服务器 `data_dir`。旧配置文件中的 `[content]` 和旧路径暂存 API 仅保留兼容读取。
- `web.enabled` 默认关闭；启用时 `web.bind` 只能是 IP 地址，默认仅允许 `127.0.0.1`/`::1` 等回环监听。
- 非回环监听必须同时设置 `web.allow_remote = true` 和公网 HTTPS `web.public_base_url`，并由反向代理终止 TLS。
- `web.admin_secret` 只用于建立短期 HttpOnly 会话，不会由插件显示或写入日志。
- `illustrations.mode = "direct"` 会读取本地图片并交给宿主按协议发送。
- 启用管理服务且使用 `direct` 模式时，插图页只能向已编译 manifest 的 `.webp` 资源键上传 PNG/JPEG/BMP/WebP；服务在 `direct_asset_root` 内重编码为 WebP，单次输入/输出上限为 8 MiB，保存后需 reload 插件才生效。
- `illustrations.mode = "remote"` 会拼接 `remote_base_url` 生成公网图片地址，适合 QQ 官方 Markdown。
- QQ 官方机器人使用远程图片时，地址必须是平台可访问的 HTTPS URL。

## Management API

启用管理服务后，`/healthz` 与 `/readyz` 可用于本机进程检查。管理 API 默认同源且不开放 CORS：

- `POST /api/v1/session`：以管理密钥建立短期会话并返回 CSRF token。
- `GET /api/v1/session`、`DELETE /api/v1/session`：读取或结束当前会话；退出请求需要 CSRF token。
- `POST /api/v1/illustrations/upload`：仅 `content_admin` 会话可写入已声明的 direct 本地插图。正文为原始图片字节，必须携带 `X-CSRF-Token` 和 `X-Illustration-Asset-Key`；不会接受路径、文件名或远程 URL。
- `GET /api/v1/content/active`：读取当前激活内容 revision，要求 `content_admin` 会话。
- `GET /api/v1/content/revisions`：读取 revision 元数据和成员数量，使用 `after_id` 游标与 `limit=1..100`。
- `GET /api/v1/content/drafts`：读取草稿状态、哈希和校验错误，使用相同游标；不返回草稿正文。
- `POST /api/v1/content/drafts/stage`：正文为不超过 2 MiB 的 UTF-8 JSON/TOML 内容包，必须携带 `X-Content-Package-Format: json|toml`、`content_admin` 会话和 `X-CSRF-Token`；解析后直接复用 Store 暂存事务，不写内容文件。未携带格式头时仍兼容旧版 `{ "package_file": "安全相对路径" }` 请求。
- `GET /api/v1/content/drafts/{package_key}/{package_revision}/diff`：读取草稿相对当前 active revision 的成员差异；不返回正文且不改变草稿状态。
- `GET /api/v1/content/activations`：读取追加式 activation 历史，使用相同游标。
- `POST /api/v1/content/drafts/{package_key}/{package_revision}/validate`：校验已暂存草稿；要求 `content_admin` 会话和 `X-CSRF-Token`，不接收草稿正文。
- `POST /api/v1/content/drafts/{package_key}/{package_revision}/publish`：发布已校验草稿；首次发布返回 `201`，重放返回 `200`，同样要求 CSRF。
- `GET /api/v1/content/operations`：读取追加式管理员操作审计，使用相同游标；不会返回会话指纹。
- `POST /api/v1/content/revisions/{revision_id}/rollback`：回滚到已存在的正 revision；要求 `content_admin` 会话和 `X-CSRF-Token`，只追加一条 `rollback` activation，返回 `201`。
- `GET /api/v1/content/rollback-operations`：读取 rollback 专用的追加式管理员审计，使用相同游标；不会返回会话指纹。
- `GET /api/v1/content/stage-operations`：读取受限暂存的追加式管理员审计，使用相同游标；不会返回会话指纹或文件路径。

内容写路由只操作上传正文、旧版兼容文件、已暂存草稿或既有 revision，并通过 Store 事务同步写入管理员审计；不返回草稿正文，也不提供目录直写。内容包上传只解析并暂存，不创建、覆盖或删除内容文件。`/api/v1/illustrations/upload` 只允许受限 direct 根、manifest 键和图片字节，不写游戏数据库、catalog、实体绑定或上传历史。rollback 不删除目录、草稿或 revision，不恢复已剥离魂环/魂技，也不会迁移或改写任何玩家、魂环或魂技状态。

## Management UI

启用 `[web]` 后，访问配置的管理端根地址（默认 `http://127.0.0.1:18181/`）即可使用内置页面。它由 shadcn/ui 的 Button、Input、Tabs、Table、Badge、Skeleton、Alert 和 Tooltip 组成，并且只请求以下既有接口：

- 会话建立、读取和退出。
- active revision、草稿、revision、activation 元数据。
- `operations`、`rollback-operations` 与 `stage-operations` 的脱敏游标分页。

页面复用既有的 stage、validate、publish、rollback、单条玩家确认和受限 direct 插图上传入口；插图上传完成后需 reload 插件才读取新字节。管理密钥仅用于登录请求；cookie 由 HttpOnly/SameSite 会话管理，CSRF token 只保留在页面内存。服务只提供 `/` 和精确的 `/assets/*` 静态资源路径，未知路径返回 404；页面 CSP 限制脚本、样式、连接和字体为同源，所有静态响应均使用 `no-store`、`nosniff`、`DENY` 和 `no-referrer`。

## Common Commands

插件会在 QimenBot 命令系统中注册游戏命令。常用入口包括：

- `斗罗系统`：查看游戏菜单。
- `开始穿越 <角色名> <男|女>`：创建角色。
- `武魂觉醒`：觉醒武魂。
- `状态`、`位置`、`背包`、`任务`、`魂兽`、`技能`、`魂环`：查看主要玩法状态。
- `挑战 <魂兽>`、`攻击`、`释放技能 <魂技>`、`逃跑`：进行 PVE 战斗。

命令前缀、群聊 @、私聊裸命令和管理员入口由 QimenBot 宿主统一配置。

## Compatibility And Runtime Boundaries

- OneBot 11 已在隔离 QimenBot `0.1.20` 宿主完成私聊、群聊、消息回复与 Base64 图片富消息验收。
- QQ 官方机器人已覆盖字符串 ID、Markdown、群/C2C 图片调度和合成 payload 回归，但尚未完成真实 Gateway 与客户端回执验收，因此当前商城版本不声明 `qq-official` 驱动兼容。
- 插件不提供 Webhook，也不读取 Bot AppID、Secret、access token 或宿主凭据。
- 插件默认不访问外部网络。仅在 `illustrations.mode = "remote"` 时生成管理员配置的 HTTPS 图片 URL；独立 `douluo-media` 服务只读取本地发布目录和媒体 catalog，由部署者自行暴露 HTTPS。
- 插件在宿主 `data_dir` 下读写固定默认位置的 SQLite 游戏数据库和受限 direct 图片目录。管理端内容包上传不落源文件；direct 图片上传只允许已编译 manifest 的资源键。
- 启用管理 HTTP 服务时会启动一个受控后台线程；`reload` 或卸载时由 `#[shutdown]` 停止并 `join`。未启用时不启动该线程。
- 游戏命令使用普通消息回复；插件没有定时主动推送、脱离事件的广播任务或常驻扫描器。

## Upgrade, Uninstall And Security

- SQLite 当前数据结构版本为 `42`。启动时按顺序执行内置结构升级并严格校验 schema；发现不兼容旧玩家身份或魂环历史时会拒绝加载，不会自动接管、转换或删除玩家状态。
- `v0.1.2` 不新增相对于 `v0.1.1` 的数据库迁移，只修复在线配置默认值和管理端内容包上传。动态库可热重载，但数据库已经升级后不承诺降级到不了解该 schema 的旧版本。
- 内容 revision 的 publish/rollback 只切换内容目录可见性，不是数据库备份或恢复功能，也不会恢复已剥离的魂环、魂技或其他玩家状态。
- 卸载插件不会删除 `data_dir` 下的 SQLite、内容包、审计记录或本地图片；需要管理员在停用插件后自行保留或清理。
- 本项目不提供自动备份、快照、容灾或跨版本数据恢复。生产升级前应由部署者按自身要求备份插件 `data_dir`。
- 普通问题通过 [GitHub Issues](https://github.com/lvyunqi/douluo-bot/issues) 反馈；安全问题请使用仓库的 [Security advisory](https://github.com/lvyunqi/douluo-bot/security/advisories/new)，不要在公开 Issue 中提交凭据或用户数据。

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
