# douluo-bot

基于 [QimenBot](https://github.com/lvyunqi/QimenBot) 动态插件 API 0.6 开发的斗罗大陆文字游戏。

## 功能

- 独立 Rust `cdylib`，支持 QimenBot 动态加载和热重载。
- SQLite 本地存档和自动数据迁移。
- 支持 OneBot 11 通用消息与 QQ 官方机器人 Markdown。
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

## 构建

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

## 安装

1. 将对应平台的动态库复制到 QimenBot `plugin_bin_dir`，默认是 `plugins/bin/`。
2. 在 QimenBot Web 插件页点击“重新扫描”。
3. 确认插件 `douluo-game` 显示 API `0.6` 且状态正常。
4. 按需在插件配置页保存配置并重新加载插件。

插件默认在 QimenBot `data_dir/douluo-game/douluo.db` 创建数据库。不要提交数据库、插件 TOML 配置或编译后的动态库。

## 配置

QimenBot 会根据仓库根目录的 `config.schema.json` 和 `config.ui.json` 生成在线配置表单。等价 TOML 示例：

```toml
[database]
relative_path = "douluo-game/douluo.db"
busy_timeout_ms = 3000

[identity]
namespace = "default"
max_character_name_chars = 6

[messages]
qq_official_markdown = true
onebot_markdown = false
legacy_hyphen_arguments = true
```

`identity.namespace` 用于隔离共享同一数据库的部署，投入使用后不要随意修改。OneBot Markdown 是实现扩展，仅应在目标客户端实际验证通过后开启。

命令前缀、私聊裸命令、群聊 @ 和回复触发由 QimenBot `[official_host.commands]` 统一控制，插件不硬编码 `/`。

## 参与开发

```powershell
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
git diff --check
```

提交代码前请确保格式化、Clippy、测试和 release 构建均通过。请勿提交 `target/`、数据库、插件配置、Token 或编译后的动态库。

## 许可证

[Apache License 2.0](LICENSE)
