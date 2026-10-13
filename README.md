# jxpan-rust

逐步将 JxPan 服务端迁移到 Rust，保留当前部署原网页，先实现天翼和移动，再逐个迁移其它网盘。

**开发中，不能直接替换旧服务。** 恢复工作请先读 [当前状态与验证](docs/STATUS.md)，再读 [实现计划与提交门禁](docs/PLAN.md)。未实现功能明确返回 501，不代理旧服务、不伪造成功。

## 已具备的能力

- 原首页、后台登录页、后台页面资源已迁入；首页/登录页通过当前部署内容 SHA-256 对比。
- 天翼和移动协议基础、只读目录/文件解析、快速直链及共享目录树。
- 有界 TTL 缓存、请求合并、共享连接池；缓存返回已序列化缓冲区，不重复复制目录树。
- 扫码/续期协议内核和专用工作线程 SQLite KV 已通过离线差分；完整路由和后台联调仍在推进。
- 移动真实目录 JSON/302/CDN Range 已验证。天翼现有样本在旧服务也返回 502，不能据此宣称成功下载闭环。

## 开发与启动

需要 Rust 1.93 或更新版本。依赖按实际使用选择最小 features，提交 Cargo.lock。

```sh
cargo fmt --check
cargo check --locked -j 2
cargo clippy --locked --all-targets -j 2 -- -D warnings
cargo test --locked -j 2
cargo run --locked
```

默认监听 `127.0.0.1:8789`，不占用旧服务端口，也不自动暴露到局域网。

| 变量 | 含义 |
| --- | --- |
| `JXPAN_LISTEN_ADDR` | IP:端口；例如 `127.0.0.1:8790`；允许端口 0 用于测试 |
| `JXPAN_CREDENTIALS_FILE` | 本机私有 JSON 文件，最多 64 KiB；不要提交到 Git |
| `MCLOUD_AUTHORIZATION` / `MCLOUD_AUTH` | 移动授权，仅在私有文件中未配置时采用 |
| `CLOUD189_TOKEN` / `P189_TOKEN` | 天翼 Open Token，仅在私有文件中未配置时采用 |

凭据文件结构：`{"mcloud":{"authorization":"..."},"cloud189":{"open_access_token":"...","session_key":"...","session_secret":"..."}}`。实际值只保存在访问权限受限的本机文件中，不通过命令行、日志或测试报告打印。

运行时初始采用单线程 Tokio，SQLite 使用独立有界工作队列。解析缓存预算 8 MiB、下载缓存 256 KiB，分别最多 64 项和 8 个独立活动加载。预算不等于整个进程的 RSS 上限；性能优劣必须同环境测量。

## 原部署差分

CI/普通 `cargo test` 使用原部署计算的**合成** fixture，不要求访问用户私有实例。显式冻结/复验工具位于 `tools/`；Node 依赖仅用于开发，不是服务运行时依赖。

```sh
npm ci --prefix tools --ignore-scripts --no-audit --no-fund
# 只有本机已具备明确授权、冻结版本一致的私有源码时运行：
JXPAN_LIVE_ORACLE=1 cargo test --locked -j 2
python3 tools/verify-pages.py
python3 tools/verify-live.py --baseline <明确授权的当前部署地址>
```

真实验证拥有并回收自己的本地服务进程，只读取 CDN 1 字节。实际账号、分享 URL、下载 URL、原部署源码都放 `.scratch/baseline/`，不进入 Git。提交前运行 `python3 tools/audit-public.py`。

不自动推送、不替换旧部署、不执行未授权的网盘转存/删除测试。页面资源来源和第三方许可见 `NOTICE` 与 `LICENSES/`。
