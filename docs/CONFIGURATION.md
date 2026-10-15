# 使用配置

## 启动

需要 Rust 1.93 或更新版本：

```sh
cargo run --release --locked
```

默认监听 `127.0.0.1:8789`，后台入口为 `/admin`。后台没有默认账号或密码，必须配置后才能登录。容器或反向代理的对外地址由部署配置决定。

## 环境变量

| 变量 | 含义 |
| --- | --- |
| `JXPAN_LISTEN_ADDR` | 监听 IP 和端口，默认 `127.0.0.1:8789` |
| `JXPAN_DB_PATH` | SQLite 文件路径，默认 `jxpan.sqlite`；使用本服务的数据库，不要指向旧服务数据库 |
| `JXPAN_ADMIN_USER` / `JXPAN_ADMIN_PASSWORD` | 后台账号和密码；兼容旧变量 `admin` / `pass` |
| `JXPAN_PUBLIC_ORIGIN` | 对外 HTTP(S) origin；反向代理或 HTTPS 部署应显式设置，不信任转发头；HTTPS 使用 Secure cookie |
| `JXPAN_CREDENTIALS_FILE` | 本机私有 JSON 配置文件，最多 64 KiB |
| `MCLOUD_AUTHORIZATION` / `MCLOUD_AUTH` | 移动云盘授权的启动回退来源 |
| `MCLOUD_ENABLED` | 字面值 `false` 禁用移动云盘解析及续期 |
| `CLOUD189_TOKEN` / `P189_TOKEN` | 天翼云盘 Open Token 的启动回退来源 |

网盘账号可在后台管理。私有凭据文件也可提供 `mcloud.authorization`、`cloud189.open_access_token`、`cloud189.session_key` 和 `cloud189.session_secret` 等配置，完整兼容规则见 [账号与授权](AUTH.md)。持久化账号优先于启动配置，已注销账号不会在重启后被旧配置自动恢复。

## 数据与安全

- 数据库保存账号、后台会话、设置、记录与统计，应限制文件权限并定期备份。
- 密码、凭据文件、数据库和实际分享/下载地址不得提交到代码仓库或输出到日志。
- 账号修改需要后台登录和同源 POST；公开状态只返回脱敏元数据。
- 受管账号在解析或取址时按需尝试续期，无后台定时保活，也没有续期关闭开关。`JXPAN_AUTH_REFRESH` 不生效；缺少续期凭据或供应商撤销授权时仍需重新登录。
- 日志默认保留 7 天，后台可设置为 1–3650 天；累计统计独立保留。

进一步说明：[授权](AUTH.md) · [存储](STORAGE.md) · [解析设置](PARSE_SETTINGS.md) · [记录](RECORDS.md) · [短链](SHORTLINKS.md)。
