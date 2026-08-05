# douluo-bot

基于 [QimenBot](https://github.com/lvyunqi/QimenBot) 动态插件 API 0.6 开发的斗罗大陆文字游戏。

## 功能

- 独立 Rust `cdylib`，支持 QimenBot 动态加载和热重载。
- 游戏插件使用 SQLite 本地存档和自动数据迁移。
- 支持 OneBot 11 通用消息与 QQ 官方机器人 Markdown。
- 支持由内容资源配置驱动的 OneBot 图片消息段与 QQ 官方 Markdown 公网 HTTPS 插图。
- 提供可独立运行的只读 `douluo-media` 图片服务。
- 角色创建、武魂觉醒和角色状态查询。

当前命令：

| 命令 | 别名 | 说明 |
|---|---|---|
| `斗罗系统` | `斗罗菜单`、`菜单` | 查看主菜单 |
| `开始穿越 <角色名> <男\|女>` | `开始转生` | 创建角色 |
| `武魂觉醒` | `觉醒` | 觉醒第一武魂 |
| `状态` | `我的状态`、`属性` | 查看角色状态 |

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

当前已在隔离 QimenBot `0.1.20` 宿主验证 OneBot 消息段和连续 10 次热重载。QQ 官方 Markdown 与图片 payload 已按适配器契约实现，但群、C2C、频道和 DMS 仍需使用真实 QQ Bot Gateway、权限和客户端分别验证。

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
max_character_name_chars = 6

[illustrations]
enabled = true
mode = "direct" # direct 或 remote
remote_base_url = ""

[messages]
qq_official_markdown = true
onebot_markdown = false
legacy_hyphen_arguments = true
```

`identity.namespace` 用于隔离共享同一数据库的部署，投入使用后不要随意修改。OneBot Markdown 是实现扩展，仅应在目标客户端实际验证通过后开启。

`direct` 模式使用资源记录中的完整 HTTPS 地址；`remote` 模式将稳定资源键拼接为 `{remote_base_url}/media/{asset_key}`。当前内置命令只绑定了稳定资源键，尚未附带直连 URL manifest，因此在 `direct` 模式下会保留完整文字但不显示内置插图。来源和许可证未核验的旧图片不会随源码发布。

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
