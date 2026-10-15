# JxPan Rust

使用 Rust 编写的网盘分享解析服务，支持 **天翼云盘** 和 **移动云盘**。

- 解析分享文件与目录，提供 JSON 结果和下载直链。
- 管理网盘账号，支持扫码登录、凭据持久化和按需续期。
- 提供网页后台、解析记录与统计、搜索分页和短链。

服务只解析并返回链接，不代理文件下载流量。

[使用配置](docs/CONFIGURATION.md) · [架构说明](docs/ARCHITECTURE.md)

第三方资源与许可见 `NOTICE`、`LICENSES/` 和 `web/vendor/`。
