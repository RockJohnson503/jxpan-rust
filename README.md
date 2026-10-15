# jxpan-rust

Rust 版 JxPan 服务端，保留原部署导入的页面并作兼容与安全修正。当前支持范围仅为**天翼云盘和移动云盘**；其它网盘尚未迁移，明确返回 `success:false/code:501`，不代理旧服务、不伪造成功。

**8788 已由 Rust 正式接管，部署与精确清理已核验（2026-10-15）。** apply、独立 verify 及独立 final audit 均通过。当前为 `jxpan-rust`，对外 `172.18.77.1:8788` 映射容器内 8789，沿用现有 Rust 数据库、账号、设置与会话，不合并旧 Miniflare 数据库。Git 提交推送按独立步骤核验，结果以 Git 历史/回执为准。恢复工作先读 [当前状态与验证](docs/STATUS.md) 和 [发布与历史部署记录](docs/TEST_DEPLOYMENT.md)；[实现计划](docs/PLAN.md) 中早期状态以当前状态页为准。

## 已具备的能力

- 原首页、后台登录页、后台页面已迁入；首页/后台仅做授权、日志及安全兼容补丁，**不再声称与旧页面字节一致**。来源和差异分别核验。
- 天翼/移动协议、只读目录与文件解析、快速直链、共享目录树及原始目录 ID 防下载索引。
- 后台登录/退出、动态授权和扫码会话、凭据持久化；后台统一“网盘账号”展示，只返回/显示安全元数据，不输出凭据或调试字段。状态读取不请求供应商；受管账号解析/取址时强制按需尝试续期。真人扫码和真实长周期续期尚未验收。
- 解析记录、统计、搜索/分页、删除和短链；日志默认保留 **7 天**，后台可设 **1–3650 天**，启动/写入/改设置及每小时清理。仅影响 Rust `parse_records`，累计统计独立保留。
- 有界 TTL 缓存、请求合并、共享连接池、专用 SQLite actor；区分 ready-cache 命中和合并请求，不将合并失败计为缓存成功。
- 移动已有真实目录 JSON/302/CDN 单字节 Range 历史正例；天翼 S005/S006 目录内文件取址已从 400 恢复为 302。**302 只证明返回直链，不证明 CDN 正文、播放或目录 ZIP**；完整下载矩阵仍未验收。此前天翼 502/认证拒绝保留为历史窗口，不再描述为当前完全无法取址。
- 天翼转存/隔离目录组件仅有默认关闭的离线内核，未接入公共解析，未执行真实创建/转存/删除。

## 开发与启动

需要 Rust 1.93 或更新版本；本轮已核验的严格 Clippy 工具链是 **1.93.1**，不据此宣称所有更新工具链 lint 都通过。依赖只启用实际需要的 features，提交 Cargo.lock。

```sh
cargo fmt --check
cargo check --locked -j 2
cargo clippy --locked --all-targets -j 2 -- -D warnings
cargo test --locked -j 2
cargo run --locked
```

本地默认监听 `127.0.0.1:8789`，不自动暴露到局域网；正式容器的外部 8788 → 内部 8789 映射与此默认值不同。

| 变量 | 含义 |
| --- | --- |
| `JXPAN_LISTEN_ADDR` | IP:端口；例如 `127.0.0.1:8790`；允许端口 0 用于测试 |
| `JXPAN_DB_PATH` | Rust SQLite 路径，默认 `jxpan.sqlite`；不要指向旧部署数据库 |
| `JXPAN_ADMIN_USER` / `JXPAN_ADMIN_PASSWORD` | 后台账号/密码；兼容读取旧环境变量 `admin` / `pass`，**没有默认账号或密码**；未配置则禁止登录 |
| `JXPAN_PUBLIC_ORIGIN` | 对外 HTTP(S) origin；反向代理/HTTPS 部署应显式配置，不信任转发头；HTTPS 使用 Secure cookie |
| `JXPAN_CREDENTIALS_FILE` | 本机私有 JSON 文件，最多 64 KiB；不要提交到 Git |
| `MCLOUD_AUTHORIZATION` / `MCLOUD_AUTH` | 移动授权的启动回退来源 |
| `MCLOUD_ENABLED` | 字面值 `false` 禁用整个移动 provider；仍禁止其解析及按需续期 |
| `CLOUD189_TOKEN` / `P189_TOKEN` | 天翼 Open Token 的启动回退来源 |

凭据文件结构：`{"mcloud":{"authorization":"..."},"cloud189":{"open_access_token":"...","session_key":"...","session_secret":"..."}}`。已持久化的授权优先于启动 seed；注销状态会阻止旧 seed 在重启后恢复账号。实际值只保存在受限的本机文件中，不通过命令行、日志或测试报告打印。授权修改需要后台会话及同源 POST，详见 [AUTH](docs/AUTH.md)。

**续期没有关闭开关。** 源码不再读取 `JXPAN_AUTH_REFRESH`；历史环境中的 `false` 不会禁用续期。初始化、首页、匿名后台检查及 `login_status` / `cloud189_status` 只读本地状态，不请求供应商；解析/取址才对已登录且启用的受管账号按需尝试。无后台定时保活，缺少可续期凭据或供应商撤销时仍可能需要重新扫码，详见 [AUTH](docs/AUTH.md)。

运行时采用单线程 Tokio，共享 HTTP 连接池，SQLite 使用独立有界工作队列。解析缓存预算 8 MiB，下载缓存与目录索引各 256 KiB，每个缓存最多 64 项及 8 个独立活动加载。预算不等于整个进程的 RSS 上限；性能优劣必须同环境测量。SIGINT/SIGTERM 支持本地服务正常关闭及 SQLite 关闭。

## 8788 正式部署与收尾边界

正式部署沿用已验证的 8789 制品：binary SHA-256 为 `f7496a9c10e0f5c576b9b6f9d5f759d6a20d3658709d441bf629c47babd4f7ed`，source 内容指纹为 `dfa14505f2c7a660e351bb6dc8d618a9fdacffdd3267549e292d3e423a57110d`；不可变 image ID 为 `sha256:03cc3c14fe8a2c07e813eb82d321e5ad57adc260cc30777881be692b5f899d71`，当前唯一项目镜像标签为 `jxpan-rust:release-f7496a9c10e0`。已核验 **351 项 Rust、6698 项前端 synthetic checks、44 项部署工具测试、9 项 verifier safety 测试、fmt 及 Homebrew Rust 1.93.1 严格 Clippy**。这些不替代真实长期续期、真人扫码或完整 CDN/浏览器矩阵。

正式容器启动时间为 **2026-10-15 16:38:01 +08:00**；独立 final audit `success:true`，核验正式身份/完整配置挂载/实际 binary、端口、DB inode/权限/env、备份及旧 legacy 树保持、页面 hash/匿名 401、旧测试入口 8789 拒绝连接；运行 `quick_check=ok`、stats `[60,45,15,13]`。9 个无关容器及其它带标签镜像保持，唯一项目 release image 与旧容器/tag 删除回执齐全。另观察到 110 个无标签中间镜像消失，结合 scoped `rmi` 推断为 Docker 自动祖先删除；不宣称全部 images 未变，原 cleanup 末尾库存断言失败回执保留。本地 8 个/远端 10 个临时 context 包已精确清理，仅删文件，脚本、工具链/缓存、凭据/private、DB/备份/legacy 归档及必要 context 均保留。收尾再次运行 Rust/前端/隔离页面/9 项 verifier safety/fmt/严格 Clippy 全通过。完整回执索引、清单和维护工具边界见 [TEST_DEPLOYMENT](docs/TEST_DEPLOYMENT.md)；2026-10-15 的 40 分享回归及更早测试数仍作为 [历史证据](docs/CLOUD189_SHARE_REGRESSION.md) 保留。

## 原部署差分与页面验证

普通 `cargo test` 使用当前部署计算的**合成** fixture，不要求访问用户私有实例。冻结/复验工具位于 `tools/`；Node 依赖只用于开发，本地 QR 编码器已经 vendored，生产不需要 npm。

```sh
npm ci --prefix tools --ignore-scripts --no-audit --no-fund
node tools/verify-frontend.mjs
# 仅在本机已有明确授权、版本一致的私有冻结源码时运行；这是本地 VM，不是在线云盘：
JXPAN_LIVE_ORACLE=1 cargo test --locked -j 2
python3 tools/verify-pages.py
# 仅在有明确的现网只读授权时比较页面来源；该地址不会自动成为生产目标：
python3 tools/verify-pages.py --baseline <明确授权的基准地址>
```

页面验证默认使用临时 SQLite、合成管理员与无供应商账号的干净环境，不靠旧刷新开关维持隔离。`tools/verify-live.py` 加载私有账号，现有 `--allow-provider-side-effects` 显式门控，只有明确授权目标与供应商副作用后才可启用：真实解析/取址可能按需续期，`JXPAN_AUTH_REFRESH=false` 已无效，不是禁续期的无副作用门禁。其历史失败结果不代表当前版本重新验收，CDN 单字节探测也不等于完整下载。

实际账号、分享 URL、下载 URL、原部署源码只放受限的 `.scratch/baseline/` 等私有目录，不进入 Git。提交前审查 tracked/untracked 公共文件并精确暂存，再由获授权的主代理运行 `python3 tools/audit-public.py`；该工具只检查暂存内容且会读取本机私有凭据作比较，不能把空暂存区通过当作整个工作区无泄露。

公共天翼下载只尝试 Open / AppShared，禁止 owned 私有文件回退；旧 folder-item 的 missing-idMap 缺陷已修复并限定请求子树，不再列作当前未修复项。安全差异、日志与短链边界见 [STATUS](docs/STATUS.md) 及组件文档。

本轮部署、独立 final audit 及精确临时文件清理已核验；Git 提交推送结果以独立历史/回执为准。已消费的迁移/清理动作不可重放，旧包缺失不能视为完整回退。操作仅限用户已授权范围，不执行未授权的网盘转存/删除，不做全局 prune 或还原旧库覆盖新数据。页面资源和第三方许可见 `NOTICE`、`LICENSES/` 与 `web/vendor/`。
