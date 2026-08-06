# douluo-bot

基于 [QimenBot](https://github.com/lvyunqi/QimenBot) 动态插件 API 0.6 开发的斗罗大陆文字游戏。

## 功能

- 独立 Rust `cdylib`，支持 QimenBot 动态加载和热重载。
- 游戏插件使用 SQLite 本地存档、自动数据迁移和追加式操作审计。
- 支持 OneBot 11 通用消息与 QQ 官方机器人 Markdown。
- 支持由公开 `assets/illustrations.json` 绑定驱动的 OneBot 图片消息段与 QQ 官方 Markdown 插图。
- `direct` 模式可从 QimenBot `data_dir` 内预加载图片，以 OneBot `base64://` 消息段发送；缺图时完整文字仍可用。
- 提供可独立运行的只读 `douluo-media` 图片服务。
- 角色创建、武魂觉醒、角色状态和当前位置查询。

当前命令：

| 命令 | 别名 | 说明 |
|---|---|---|
| `斗罗系统 [页码\|开始\|角色\|世界]` | `斗罗菜单`、`菜单` | 查看分页菜单；不带参数显示“开始游戏” |
| `开始穿越 <角色名> <男\|女>` | `开始转生` | 创建角色 |
| `武魂觉醒` | `觉醒` | 觉醒第一武魂 |
| `状态` | `我的状态`、`属性` | 查看角色状态 |
| `位置` | `地图`、`当前位置` | 查看当前地图和地图插图 |
| `授权上下文 <group\|channel> <ID> [标签]` | `新增授权`、`授权群` | Owner 私聊授权群或频道；旧格式 `授权群 <群号>` 仍可用 |
| `取消授权 <group\|channel> <ID> 确认` | `撤销授权`、`删除授权` | Owner 私聊撤销群或频道授权 |
| `查看授权 [下一页游标]` | `授权列表` | Owner 私聊分页查看当前 Bot 的授权上下文 |

## 兼容性

| 项目 | 要求 |
|---|---|
| QimenBot | `0.1.18+`，开发基线 `0.1.20` |
| 动态 ABI | `0.6` |
| Rust | `1.89+`，edition 2024 |
| Windows | `x86_64-pc-windows-msvc` |
| Linux | GNU `x86_64` / `aarch64` |
| musl | 不支持动态插件加载 |

插件必须与宿主的操作系统、CPU 架构和 C 运行时匹配。

当前已在隔离 QimenBot `0.1.20` 宿主验证配置 v4、OneBot 本地 Base64/远程 URL 消息段、QQ 合成事件文字降级和热重载。QQ 官方 Markdown 与图片 payload 已按适配器契约实现，但合成事件不等同于真实平台；群、C2C、频道和 DMS 仍需使用真实 QQ Bot Gateway、权限和客户端分别验证。QQ 官方首版对本地内联图片保留完整 Markdown/文字，不与独立媒体段混发。

`斗罗系统` 支持 `1`/`开始`、`2`/`角色` 和 `3`/`世界` 三个分页入口。菜单只列出当前已经可用的命令；QimenBot 自带的 `/help` 会按插件声明的导航、角色和世界分类分页展示全局命令。

## 构建

构建游戏插件：

```powershell
cargo fmt --check
cargo clippy -- -D warnings
cargo test --locked
cargo build --release --locked
```

Windows 产物：

```text
target/release/qimen_dynamic_plugin_douluo_game.dll
```

Linux 产物：

```text
target/release/libqimen_dynamic_plugin_douluo_game.so
```

构建图片服务：

```powershell
cargo fmt --manifest-path services/douluo-media/Cargo.toml -- --check
cargo clippy --manifest-path services/douluo-media/Cargo.toml --locked --all-targets -- -D warnings
cargo test --manifest-path services/douluo-media/Cargo.toml --locked
cargo build --manifest-path services/douluo-media/Cargo.toml --release --locked
```

Windows 产物为 `services/douluo-media/target/release/douluo-media.exe`，Linux 产物为同目录下的 `douluo-media`。

## 安装

1. 将对应平台的动态库复制到 QimenBot `plugin_bin_dir`，默认是 `plugins/bin/`。
2. 在 QimenBot Web 插件页点击“重新扫描”。
3. 确认插件 `douluo-game` 显示 API `0.6` 且状态正常。
4. 按需在插件配置页保存配置并重新加载插件。

插件默认在宿主传入的 `PluginInitConfig.data_dir` 下使用 `douluo-game/douluo.db`。实际根目录由 QimenBot 宿主决定；部署时应使用持久化且权限受限的工作目录。不要提交数据库、插件 TOML 配置或编译后的动态库。

## 配置

QimenBot 会根据仓库根目录的 `config.schema.json` 和 `config.ui.json` 生成在线配置表单。等价 TOML 示例：

```toml
[database]
relative_path = "douluo-game/douluo.db"
busy_timeout_ms = 3000

[identity]
namespace = "default"
# 仅用于没有 qimen_context 的旧版 QimenBot 单官方 QQ Bot 回退；新版宿主会校验两者一致。
qq_official_account_id = ""
max_character_name_chars = 6

[authorization]
mode = "allow_all" # allow_all 或 allowlist；私聊始终允许

[illustrations]
enabled = true
mode = "direct" # direct 或 remote
direct_asset_root = "douluo-game/assets"
remote_base_url = ""

[messages]
qq_official_markdown = true
onebot_markdown = false
legacy_hyphen_arguments = true
```

`identity.namespace` 用于隔离共享同一数据库的部署，投入使用后不要随意修改。OneBot Markdown 是实现扩展，仅应在目标客户端实际验证通过后开启。

新版本宿主会在规范化事件中提供稳定的 `qimen_context.account_id`，用于隔离同一发送者在不同 Bot 上的存档。缺少可验证账号时，插件会拒绝有状态命令，不会使用部署实例别名或 `unknown` 作为账号。`identity.qq_official_account_id` 不是通用多 Bot 方案，只为旧版宿主的单 Bot 官方 QQ 部署保留。

`authorization.mode = "allow_all"` 保持默认兼容，所有群聊和频道可用；私聊始终允许。切换为 `allowlist` 后，群聊和频道必须先由机器人 Owner 在私聊中执行 `授权上下文 <group|channel> <ID> [标签]`，否则游戏命令会被拒绝且不会执行业务逻辑。OneBot 只支持 `group` 授权；QQ 官方机器人支持 `group` 和 `channel`，C2C 与 DMS 视为私聊。授权记录按协议、稳定 `account_id` 和 `identity.namespace` 隔离，撤销时必须输入 `确认`。

`assets/illustrations.json` 是源码内的逻辑绑定清单，包含地图、武魂、魂兽和魂环共 19 条稳定 `asset_key`；它不包含图片二进制、绝对路径、来源取证或许可证结论。将已审核的文件按清单 key 放在 `data_dir/douluo-game/assets/` 下，插件启动时会有界读取并校验扩展名与文件签名。目录不存在、文件缺失或不合规时自动保留完整文字。

`direct` 模式优先使用上述本地图片并以内联 Base64 图片段发送给 OneBot；清单未来也可以为单个绑定提供审核后的公网 HTTPS `direct_url`。`remote` 模式将稳定资源键拼接为 `{remote_base_url}/media/{asset_key}`，适合部署独立 `douluo-media` 和公网 HTTPS 反向代理。QQ 官方 Markdown 只嵌入 HTTPS URL；本地 Base64 图片在首版统一降级为完整 Markdown/文字，不会发送混合字段。

插件不会把原始图片字节放进 Debug 输出。QimenBot 的 `qimen_raw_message=debug` 可能记录完整 OneBot 出站 JSON，其中包含 Base64；排查协议时应临时开启并在日志访问控制下使用。

命令前缀、私聊裸命令、群聊 @ 和回复触发由 QimenBot `[official_host.commands]` 统一控制，插件不硬编码 `/`。

## 图片服务

`douluo-media` 当前是 filesystem-only 的只读服务。`DOULUO_MEDIA_ROOT` 必须指向专用、运行期只读的已发布图片目录；服务在启动时递归建立内存索引，目录内容改变后需要重启。它不会提供上传、审核、媒体 SQLite catalog、转码、图片变体或 S3 存储。

```powershell
$env:DOULUO_MEDIA_ROOT = "C:\path\to\published"
$env:DOULUO_MEDIA_BIND = "127.0.0.1:18182"
./services/douluo-media/target/release/douluo-media.exe
```

`DOULUO_MEDIA_BIND` 默认为 `127.0.0.1:18182`。可用路由：

| 方法 | 路径 | 说明 |
|---|---|---|
| `GET` / `HEAD` | `/healthz` | 进程存活检查 |
| `GET` / `HEAD` | `/readyz` | 启动索引就绪检查 |
| `GET` / `HEAD` | `/media/{asset_key}` | 稳定资源键 |
| `GET` / `HEAD` | `/media/sha256/{digest}.{ext}` | 内容哈希地址 |

服务只索引不超过 20 MiB、扩展名与文件签名一致的 PNG、JPEG、WebP、GIF 和 BMP。每次发送前会重新核对实际字节的 SHA256，逻辑地址使用短缓存，哈希地址使用不可变缓存，并支持 ETag、Last-Modified、单 Range 和条件请求。

服务本身只监听 HTTP。QQ 官方 Markdown 图片必须使用 QQ 平台可访问的公网 HTTPS 地址，生产环境应让 Caddy、Nginx 或 CDN 终止 TLS，再把请求反向代理到回环监听。服务最多同时持有 4 个图片响应缓冲，额外读取返回 `503`；反向代理应配置连接/响应超时和限流。推荐使用无路径基址，例如 `remote_base_url = "https://media.example.com"`；若配置 `https://media.example.com/douluo`，反向代理必须去掉 `/douluo` 前缀，使外部 `/douluo/media/*` 映射到服务的 `/media/*`。

## 参与开发

```powershell
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
cargo fmt --manifest-path services/douluo-media/Cargo.toml -- --check
cargo clippy --manifest-path services/douluo-media/Cargo.toml --locked --all-targets -- -D warnings
cargo test --manifest-path services/douluo-media/Cargo.toml --locked
cargo build --manifest-path services/douluo-media/Cargo.toml --release --locked
git diff --check
```

提交代码前请确保格式化、Clippy、测试和 release 构建均通过。请勿提交 `target/`、数据库、插件配置、Token 或编译后的动态库。

## 许可证

[Apache License 2.0](LICENSE)
