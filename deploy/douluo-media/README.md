# douluo-media 部署

`douluo-media` 是只读图片服务。它启动时扫描专用 published root，运行期间不上传、改名、删除或更新图片；更换资源后必须重启服务重新建立索引。

## 文件

- `douluo-media.service`：Linux systemd 单实例服务，限制进程权限并把 published root 标记为只读。
- `Caddyfile`：公网 HTTPS 读取面；`MEDIA_BASE_PATH` 会被 strip 后再转发到内部固定的 `/media/*`。
- `scripts/media-remote-smoke.ps1`：本地启动媒体服务、Caddy 和隔离 QimenBot，验收只读 root、HTTPS、base-path 和插件 `remote` URL。

## 部署

先构建并安装二进制，目录只放已经审核发布的图片：

```bash
cargo build --manifest-path services/douluo-media/Cargo.toml --release --locked
install -D -m 0755 target/release/douluo-media /usr/local/bin/douluo-media
install -d -o douluo-media -g douluo-media -m 0755 /srv/douluo-media/published
```

先复制 `douluo-media.service` 到 `/etc/systemd/system/`，确认 `User`、二进制路径和 root 路径。将发布目录作为只读 bind mount 提供给服务。宿主准备新版本目录后，使用独立的 mount 更新流程切换目录，再重启服务；不要在运行中的 published root 内覆盖文件：

```bash
mount --bind /srv/douluo-media/releases/<revision> /srv/douluo-media/published
mount -o remount,bind,ro /srv/douluo-media/published
systemctl daemon-reload
systemctl enable --now douluo-media.service
```

服务仅监听回环 `127.0.0.1:18182`，公网读取必须经过 Caddy/Nginx 等反向代理。

## Caddy

为 Caddy 提供以下环境变量，并把 `CADDY_STORAGE_ROOT` 放在可写的证书/运行数据卷；它不能指向 published root：

```text
MEDIA_HOST=media.example.com
MEDIA_TLS=
MEDIA_BASE_PATH=/douluo
MEDIA_UPSTREAM=127.0.0.1:18182
CADDY_STORAGE_ROOT=/var/lib/caddy
```

使用仓库中的 `Caddyfile` 启动 Caddy。公网 `MEDIA_HOST` 使用真实域名时不要设置 `MEDIA_TLS=tls internal`；Caddy 会自动申请 HTTPS 证书。`MEDIA_BASE_PATH=/douluo` 对应插件配置中的完整基址：

`Caddyfile` 的 `skip_install_trust` 只避免本地 `tls internal` 冒烟把临时 CA 写入宿主信任库；它不影响公网证书申请，但也不应把内部临时证书作为公网图片服务的证书来源。

```toml
[illustrations]
enabled = true
mode = "remote"
remote_base_url = "https://media.example.com/douluo"
```

插件只生成稳定的 `asset_key` URL，不在同步 FFI 回调中探测媒体服务。媒体服务不可用时消息按插件现有文字降级；已经提交的游戏动作不会因图片请求失败回滚。

## 更新边界

只读 root 的文件变化不会热加载。推荐顺序是：准备新 revision 目录、执行 `media-remote-smoke.ps1` 或同等验收、切换只读 mount、重启 `douluo-media`、检查 `/readyz`，最后确认插件消息中的 URL 和公网 HTTPS 响应。此切片不增加 SQLite catalog、上传接口、管理密钥、玩家状态迁移或 QimenBot 宿主改动。
