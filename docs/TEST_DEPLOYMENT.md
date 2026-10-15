# 8788 正式部署与独立实机测试历史

> **8788 已由 Rust 正式接管，部署与精确清理已核验。** apply、独立 verify 及独立 final audit 均通过；原 cleanup 末尾库存断言失败的回执仍保留，不改写为成功。Git 提交推送按独立步骤核验，结果以 Git 历史/回执为准。真实扫码、长期续期及完整下载矩阵仍有独立验收边界。

## 8788 正式部署（2026-10-15）

| 项目 | 已知状态 / 验收边界 |
| --- | --- |
| 来源 | 沿用已验证的 Rust 8789 最终制品，以同一不可变 image ID 正式接管 8788 |
| binary SHA-256 | `f7496a9c10e0f5c576b9b6f9d5f759d6a20d3658709d441bf629c47babd4f7ed` |
| source 内容指纹 | `dfa14505f2c7a660e351bb6dc8d618a9fdacffdd3267549e292d3e423a57110d`；不是 Git tree hash |
| 本地验证 | 351 项 Rust、6698 项前端 synthetic checks、44 项部署工具测试、9 项 verifier safety 测试、fmt 及 Homebrew Rust 1.93.1 严格 Clippy 已通过；收尾再次运行 Rust/前端/隔离 verify-pages/9 项 verifier safety/fmt/严格 Clippy 全部通过 |
| 支持范围 | 仅天翼 + 移动；其它网盘 `success:false/code:501`，不代理旧服务 |
| 当前行为 | 统一“网盘账号”UI，无原始凭据/调试信息；受管账号解析/取址强制按需续期，状态读取不请求供应商；移动禁用仍禁止解析及续期 |
| 真实下载证据 | 天翼 S005/S006 目录内文件取址由 400 恢复 302；不等于 CDN 正文、播放、目录 ZIP 或完整下载矩阵通过 |
| 正式容器 | `jxpan-rust`；ID `6b84cd5b2d61e31dfcd4d8e6221d838b162b21b076a793d1e99a655617cf5ed6` |
| StartedAt | `2026-10-15T08:38:01.181710027Z`，即 **2026-10-15 16:38:01 +08:00** |
| 不可变 image ID / 唯一项目 tag | `sha256:03cc3c14fe8a2c07e813eb82d321e5ad57adc260cc30777881be692b5f899d71` / `jxpan-rust:release-f7496a9c10e0` |
| 正式端口 / origin | `172.18.77.1:8788` → 容器内 8789；`PUBLIC_ORIGIN` 对齐外部 8788 |
| 部署验证 | apply、独立 verify 及独立 final audit（`success:true`）通过，正式身份、实际 binary、全配置/挂载、端口、页面/权限、DB 与备份完整性通过 |
| 尚未验收 / 独立核验 | 真人扫码、真实长周期续期、完整浏览器/供应商及下载矩阵；Git 提交推送以独立 Git 历史/回执为准 |

上述制品及验收结果以对应部署、验证、审计回执为证据；source 内容指纹不是 Git tree hash。Git 基点 `25b97f253266e6aa497fef763ab1ac05271ba062` 后包含构建时工作区改动，不能仅凭该基点重建同一制品。

### 已完成的正式接管与数据保护

- `/opt/jxpan-rust-test` 原子 rename 到 `/opt/jxpan-rust`，真实 DB inode **18885230** 保留；未复制旧库冒充原库。停服后的新鲜只读备份保留，未恢复回写；交接全部表和保护 KV 一致，运行 `quick_check=ok`、stats `[60,45,15,13]` 未下降，当前 accounts/settings/session 保留，不 reset/扣减统计或替换登录。
- 旧 8788 Miniflare 数据和镜像分别归档为 `/opt/jxpan-rust-migration-20261015-f7496a9c10e0-continuation1/legacy-data.tar.gz`、`legacy-image.tar`。原 `/opt/jxpan` 未改；**不 merge 旧 DB**，也不把旧备份覆盖到当前 Rust DB。
- 初次迁移因 legacy 收到 SIGTERM 后退出 1 而被保护逻辑停下，旧失败证据保留；续作单独审阅后执行，成功 apply/verify 和失败阶段均保留，不重跑旧 attempt。不能因后续成功就改写初次失败回执。
- 正式入口映射、实际运行制品和页面/权限由主代理独立核验。真实 QR 登录、长期续期、CDN 正文和完整 provider 矩阵本次未新跑。状态读取不请求供应商；真实解析/取址可能按需续期，`JXPAN_AUTH_REFRESH=false` 无效，`tools/verify-live.py` 须显式 `--allow-provider-side-effects` 并事先获得相应授权；`verify-pages.py` 默认 synthetic、无 provider 账号。

### Docker 清理与独立 final audit：已核验

- 已按精确清单移除 **14 个旧项目容器、11 个旧项目 image ID、12 个旧 tag（含当前测试别名）**；独立审计核对 14 个容器删除回执与 12 个 tag 回执齐全，唯一项目 release image 为上表正式制品。历史“旧容器/镜像留存”不再是当前库存或可直接启动的回滚方案；代码回退须使用受控制品/重建途径，不恢复旧 DB 覆盖新数据。
- **9 个无关容器的完整 Config、HostConfig、Mounts 及运行状态不变，其它带标签镜像不变**，没有全局 prune。另观察到 **110 个无标签、约 8.5–8.8 MB 的中间镜像消失**；结合本次 scoped `rmi`，推断为 Docker 默认自动删除祖先中间镜像，并非逐项证实其删除机制。不能宣称全部 images 未变。原 `cleanupfailed-at-final-inventory-assertion` 失败回执保留，后续独立审计成功不改写原结果。
- 独立 `.scratch/promote-8788/final_audit.py` 已成功，final audit 为 **`success:true`**：核验正式容器 ID/StartedAt、完整 Config/HostConfig/Mounts、实际 binary、外 8788/内 8789、当前 DB inode、权限和 env；同时核验上述无关容器/带标签镜像、唯一项目 release image、清理回执、备份 tar/SQLite 完整性及旧 legacy 树保持。运行 DB `quick_check=ok`、stats `[60,45,15,13]`；旧测试入口 8789 拒绝连接，正式首页/登录页/QR hash 与匿名私有接口 401 均通过。此处的 8789 拒绝连接指宿主旧测试入口，不是容器内监听端口。
- 远端独占创建的 0600 审计回执：`/opt/jxpan-rust-migration-20261015-f7496a9c10e0-continuation1/post-cleanup-audit-v1.json`；本地脱敏索引：`.scratch/promote-8788/final-audit-result.json`。私有回执内容不提交公共仓库。

### 临时 context 包精确清理：已完成

- 本地仅删除以下 **8 个** `.scratch/<阶段>-8789/context.tar`：`deploy`、`ttl`、`season1`、`concurrency`、`concurrency-empty`、`concurrency-fix`、`cloud189-fix`、`cloud189-download-qr`，合计 **69,140,480 B**。
- 远端仅删除 `/opt/jxpan-rust/context.tar` 及 `/opt/jxpan-rust/releases/{1807efaa0aed,2cd8947b306d,2f223a23f1ab,3d1cee097916,48b067c74807,84743f5d425e,998b1c3825b2,db1165cf1d26,e897dd1e919a}/context.tar`，共 **10 个、86,446,080 B**。只删除文件，未删除目录。
- 保留本地 latest UI 与直接前版 auth-refresh 的 context 包、远端当前 release `f7496a9c10e0` 的 context 包；全部阶段脚本、attempt/result 回执、测试凭据、private、数据库/备份和 legacy 归档均保留。`.scratch/deploy-8789/toolchain` 与 `.scratch/concurrency-fix-8789/zig-{global,local}-cache` 工具链/缓存明确保留。
- 精确清单及结果：本地 `.scratch/promote-8788/artifact-cleanup-{manifest,result}.json`；远端迁移目录下 `artifact-cleanup-result.json` 为受限回执。已消费的迁移/清理动作不可重放，旧 context 包已缺失，保留脚本不等于完整回退能力。

### 维护工具与 Git 证据边界

通用调试工具保留：`tools/verify-frontend.mjs`、`tools/verify-pages.py`（默认 synthetic）、`tools/test-verification-safety.py`、`tools/verify-live.py`（仅明确授权目标及供应商副作用后使用）。各阶段 `.scratch/` 脚本与 attempt/result、新增部署动作/诊断脚本均本地保留，私有文件不提交。历史脚本只供核对，不可盲目重放已消费动作。

部署、独立 final audit 及临时文件精确清理均已核验；Git 提交/推送单独执行与核验，不从部署成功推导 Git 成功，最终结果以 Git 历史/回执为准。原失败及续作证据保持。

提交前检查全部待提交 tracked/untracked 公共文件，精确暂存并审查暂存差异后运行 `tools/audit-public.py`；该工具仅检查 staged 内容且读取本机私有凭据，空暂存区通过不等于全工作区审计通过。不要 `git add .` 或强制加入 ignored scratch。

## 历史部署窗口（不代表当前运行/库存）

以下保留各阶段版本、测试数、当时的失败、备份与回滚容器记录。“8788 未动”“不替换旧服务”“禁用刷新”“旧镜像保留”只描述其历史窗口；当前正式部署、清理结果及独立验收边界以上方为准。历史 `/opt/jxpan-rust-test` 下证据路径是当时索引，目录迁移后应在 `/opt/jxpan-rust` 下核对，不据旧路径重建/重跑。私有回执不向公共仓库发布内容。

## 2026-10-15 强制自动续期发布边界（历史 8789 窗口）

- 只更新 `jxpan-rust-test` / 8789；天翼与移动解析/取址强制启用受管续期，代码无关闭开关。沿用私有 env-file 的字节，Docker CLI 仅覆盖 `JXPAN_AUTH_REFRESH=true`，其余 ENV/完整挂载/资源限制不变。
- `.scratch/auth-refresh-8789/` 四脚本由父代理审核 digest 后冻结；五文件 context、离线构建、固定目标 SSH、各阶段独占 0600 回执。旧容器及既有两级回滚保持停止且 restart=no，回滚只能切代码，不能恢复旧 DB/授权。
- 停止旧测试实例后，从刚停止的当前 DB 新建远端 0600 SQLite 备份；create 新容器、验证唯一 ENV 差异及全库交接内容相等后才 start。运行后允许用户合法刷新/登录/记录变化，仅检查完整性及累计统计不倒退；不为比对还原、扣减数据。
- 验证只访问首页、匿名登录页、QR 静态资源和匿名私有接口 401，不强制调用真实续期、解析或下载。离线内存/loopback 回归覆盖强制路径、一次重试、禁用 provider、冷却/退避、账号切换、落库失败及缓存/统计边界。真实长周期到期续期仍待后续使用验证，不把 synthetic 当真实续期成功。
- 前一下载/移动扫码版本 `test-84743f5d425e` 的回执保留在 `.scratch/cloud189-download-qr-8789/`：S005/S006 目录内文件取址从 400 恢复为 302；这是取址，不是目录 ZIP、文件正文下载或真人移动扫码确认。

## 2026-10-15 天翼分享兼容、签名与认证分类（历史运行版本）

用户要求修复天翼大量 502 并从 PanSou 找不同类型分享测试。仅将 8789 从 `test-e897dd1e919a` 更新，源代码相对上一部署仅 Cloud189 client/service 与 HTTP reply 改变；未改移动、前端、设置或凭据。

- 镜像 tag：`jxpan-rust:test-3d1cee097916`；实际启动 image ID：`sha256:987a058ed7cd89f54d11f9bdaa9180cc3eb980c351e1117739e773ef97565d1e`，容器 Config.Image 也为该 ID。
- 实际 binary SHA-256：`3d1cee097916854b2dca77dddc2ebacff2c1996c60f6095d2b797c9fad5004ff`，**8631368 字节**，静态 Linux x86_64；source SHA-256：`e1db2d8dae1f8f0f4ceaff7c176251707d8ef23861f0ccbc7f5c461ab36ab9c0`；五项 context SHA-256：`7419cfba3e9b13efead44f8b77041c89212755a507dd777c02fb63c8728fd591`。
- 288 项 Rust、fmt、Homebrew Clippy 1.93 严格 all-target、3086 前端 synthetic checks、隔离页面、安全权限、QR、HEAD/OPTIONS 通过；deployer 48 项、probe runner 32 项离线测试通过。合成验证不冒充供应商成功下载。
- 额外严格 Clippy 1.94.1 检查在未改的 `src/http/mod.rs:75` 闭包触发 result_large_err；Rust 1.94 编译/288 测试通过，但不能宣称所有工具链 lint 全绿。未修改无关 runtime source 或压制 lint，两个工具链结果见 `local-verification.json`。
- 先核验旧测试 binary/source/挂载/资源配置、8788 worker 与未来五分钟历史过期保护；正常停止后 SQLite backup/quick_check。备份路径 `/opt/jxpan-rust-test/releases/3d1cee097916/before-update.sqlite`，0600，不导出、不恢复回写。
- 切换全部表内容相同，页面/权限、实际运行 binary 和不可变 image ID 核验通过。最终只读审计仍全部表内容与切换前相同，stats 四项增量及 history 增量均 0，retention/session/provider credentials/private env/seed 保留；没有 reset/扣减、登录/刷新/session 替换或 setting write。
- 2026-10-15 当前保存并发只读观察为 **16**；这是已保存值且本轮全库保持，不是本次把 4 覆盖成默认 16。10-14 保存 4 的历史回执保留；探针自身固定 4 控制负载，未写回设置。
- `jxpan-rust-test-prev-e897dd1e919a` 停止且 restart=no 留存。只供代码回滚，不能与当前服务同时挂同库运行；未做线上回滚演练。原 8788 ID/image/StartedAt/worker 均保持。
- 独立隔离 probe 按服务不可变 image ID 运行，非 root/read-only/cap-drop ALL/no-new-privileges/256 MiB/2 CPU/64 PID，仅只读挂公开探针，不挂 DB/private、不启动服务；凭据目标机只读加载，经 stdin 注入，输出仅脱敏摘要。所有实验容器已移除。
- PanSou 九组关键词 4029 条去重前结果，40 个不同分享 / 44 样例。19 个不同完整目录树（20 个样例），最大 432 目录 / 3357 唯一文件；20 次 fresh-client fast 和 7 个单文件 parse 仍认证拒绝，下载地址 0，CDN 0。实际 parse code 200×20、401×7、404×14、403×2、400×1，无 5xx 样例不代表成功下载；这些为生产 service 探针，不是 44 次在线 HTTP endpoint 请求。
- 脱敏回执：`.scratch/cloud189-fix-8789/{release,apply-result,verify-result,final-audit,final-report,local-verification}.json` 与 `regression-*-run-result.json`；远程 `/opt/jxpan-rust-test/releases/3d1cee097916/{result,verify-result}.json`。私有样本为0600，不提交，没有 stage/commit/push。

该历史窗口的下一步为用户在 8789 重新登录天翼后补下载地址正例；后续 S005/S006 已恢复 302，见本页当前正式部署部分，不能把历史认证失败视作当前完全无法取址。Open 固定宽度签名、公开分享附带无关密码及安全运行器拒绝诊断见 [CLOUD189_SHARE_REGRESSION](CLOUD189_SHARE_REGRESSION.md)。真实 >200 同级分页、CDN、完整浏览器/认证矩阵仍待独立验收。

## 2026-10-14 首次部署记录

用户明确授权在目标机 **8789** 部署独立测试实例；**不替换 8788 旧服务**。这是未提交工作区的测试构建，不代表迁移已验收或可替代旧服务。

- 容器：`jxpan-rust-test`；目录：`/opt/jxpan-rust-test`。
- 镜像：`jxpan-rust:test-533554636198`；runtime-only `scratch` 镜像约 8.6 MB。
- 编译：Rust 1.94.1、cargo-zigbuild 0.23.4、Zig 0.14.1，`--release --locked --target x86_64-unknown-linux-musl -j2`；已确认 x86-64 静态 ELF。
- Git 基点：`25b97f253266e6aa497fef763ab1ac05271ba062`；包含当时未提交代码，不能仅用 HEAD 重建同一版本。
- 可执行文件 SHA-256：`533554636198e5f2ab6026caf3ebbab010e9b48562b195db74ec0467cad4e2e2`。
- Cargo/src/web 内容指纹：`84470a4511c5b9d8c3afea43e4f92b034fccde3f0a6ff0794595705d2b8f3f3d`；不是 Git tree hash。

## 2026-10-14 单点击重复解析热修

用户确认修复并更新 8789。应用层唯一改动是移除解析按钮的重复 inline onclick；后端计数、历史、缓存、凭据及配置代码未改。

- 热修镜像：`jxpan-rust:test-2cd8947b306d`。
- 可执行文件 SHA-256：`2cd8947b306d9e5b77d041ad03e97be1c391c09fc9601a10099ff97fbfe639d8`。
- Cargo/src/web 内容指纹：`96667d93efc579499cde6c074a32c745200d5eedcba5b06b7dbf98cb1be9837b`。
- 测试库备份：`/opt/jxpan-rust-test/releases/2cd8947b306d/before-update.sqlite`，受限目录/0600；`quick_check` 通过，不上传、不输出其数据。
- 旧测试容器：`jxpan-rust-test-prev-533554636198`，已停止、restart=no，仅供代码回滚；它与现版本挂载同一个测试库，不能同时启动。旧镜像和初始 release 元数据仍保留。
- 正常 SIGTERM 停止后备份；升级前后全部表内容指纹相等，既有统计/历史/保留期/管理员 session/网盘授权均保留。不导入初始 seed 覆盖数据，也不重置或回扣误计统计。
- 175 Rust、严格 clippy、2888 前端合成 checks、隔离页面验证通过。真实 Chrome 加载更新页后一次按钮点击仅发起一个被拦截的 synthetic parse 调用，实际后台诊断解析请求为零；未进行供应商或云盘写入。
- 8788 容器 ID/镜像/启动时间和 worker SHA 未变。浏览器需刷新加载新 HTML；缓存是进程内存状态，测试容器重启后重新建立，不等于持久统计被清零。
- 切换后的稍晚只读检查发现在线数据继续变化（管理员会话摘要、记录和累计统计）；因此不能把“切换瞬间全部表一致”扩展为“测试期间数据永远不变”。最初的延后全库相等断言未通过，已只读诊断；后续分别核验已通过的切换回执、在线累计统计不下降、页面与权限、运行状态，不回写备份或还原用户后续操作。请求诊断只拦截合成调用，未发送后台解析。

## 2026-10-14 直链缓存对齐

用户确认将直链缓存改回原版的 **10 分钟**并更新 8789。运行时代码仅将 `AppState.download_cache` TTL 从 60 改为 **600 秒**；容量、凭据隔离、解析/目录 TTL、认证刷新策略和日志保留期未改。前端仍保留上一轮单点击修复。

- 本次镜像：`jxpan-rust:test-db1165cf1d26`。
- 可执行文件 SHA-256：`db1165cf1d266ef8a7e8a520f9e3847954a66b58eb19cbee8772225f2a038a04`；已通过 Docker 只读复制实际运行 binary 核验，不只依赖标签。
- Cargo/src/web 内容指纹：`e738026cf3de49e25cf13d430fef0f0f41366cfbd01dbf93de705300ee0e4e19`。反向还原 TTL 行及新增测试后与上一运行版本指纹相同，确认无额外运行时改动。
- 测试库备份：`/opt/jxpan-rust-test/releases/db1165cf1d26/before-update.sqlite`；受限目录/0600，SQLite backup 与 `quick_check` 通过，不回写线上库。
- 旧测试容器：`jxpan-rust-test-prev-2cd8947b306d`，已停止且 restart=no；更早的 `prev-533554636198` 同样保留。回滚容器共用测试库，不得同时启动。
- 切换前核验现版本实际 binary、8788 身份、环境和挂载，以及未来五分钟内没有待过期历史，避免启动保留期清理影响此次数据保持检查。正常退出后备份；切换前后全部表内容相同，统计/历史/保留期/后台 session/网盘授权保留。私有 runtime.env 与 seed 指纹未变；未重置数据或登录替换管理员会话。
- **176 Rust 测试**、fmt、严格 clippy、**2888 前端合成 checks**、隔离 HTTP 页面验证通过。新增测试使用真实 AppState 配置和确定性时钟，验证 61 秒与 599.999 秒仍保留、600 秒失效、读取不续期及既有预算；这不是在线等待十分钟或供应商签名有效期验收。
- 更新后首页/匿名登录/QR 资源、私有设置 401、实际 binary、运行状态与数据库完整性通过。紧随更新的只读验证未发现新的表内容变化；不把此结论扩展成在线数据永不变化。8788 容器 ID/镜像/启动时间与 worker SHA 未变，本次部署和验收未发出供应商/下载请求。
- 回执：`/opt/jxpan-rust-test/releases/db1165cf1d26/{result.json,verify-result.json}`；构建及脱敏结果在本机 ignored `.scratch/ttl-8789/`。旧容器/镜像和备份保留，没有 commit/push。

十分钟仅对齐原版应用缓存策略，不等于上游直链保证有效十分钟。缓存仍是进程内存状态，重启后首次请求需要重新建立；持久统计不会因此清零。

## 2026-10-14 移动分享空目录修复（历史运行版本）

用户提供的真实分享中 `Season 1` 有两个 MP4，修复前 Rust 子目录 action 返回 404，未改变统计或记录。只读调用供应商列表接口发现：标准单层 CBC 解密后 JSON 完整，原版二次去填充误删 125 字节，正则救援再被文件名中的 `]` 截断。原类也返回空列表；这是明确修复旧缺陷，而非声称继续复制该错误。

- 该次镜像：`jxpan-rust:test-2f223a23f1ab`。
- 实际 binary SHA-256：`2f223a23f1ab993431427029b733da40ceb6dc778c2c8267808dd6deecf95c69`；8601080 字节，静态 Linux x86_64。
- Cargo/src/web 内容指纹：`e3a44b619294ae0d4280ddd4fbcf841707b40968634cfe3de458f3abe3eb1728`；与上一部署相比仅 `src/providers/mcloud_client.rs` 改变。生产响应先接受完整单层 JSON，不完整时仍走原 legacy；请求体、目录字段/遍历、前端、600 秒 TTL、认证刷新及 retention 均未改。
- **177 项 Rust 测试**、fmt、严格 clippy、2888 前端合成 checks、隔离页面验证通过。新增真实 loopback 加密 HTTP → 完整目录树/下载地址回归先红后绿，严格比对两个带方括号 MP4 的名称、ID、大小；原双层 AES/HTTP 向量保持通过。
- 正常退出后受限 SQLite backup：`/opt/jxpan-rust-test/releases/2f223a23f1ab/before-update.sqlite`，0600、`quick_check` 通过；切换前后全部表内容相同，统计/历史/保留期/后台 session/网盘授权与 private env/seed 保留，不恢复数据库或替换用户会话。
- 更新后实际 binary、页面资源、匿名私有设置 401、运行状态及 SQLite 完整性核验通过；8788 身份、启动时间和 worker 不变。上一个测试容器 `jxpan-rust-test-prev-db1165cf1d26` 已停止、restart=no；更早回滚容器也保留，共用测试库不能同时运行。
- 真实样本验收：普通解析返回 200，`Season 1` 正确列出 **2 个 MP4**，名称/原始 ID/字节大小与完整上游 JSON 一致；两条直链请求均 **302**，不跟随 Location、CDN 请求为 **0**。这不是视频下载/首帧验证，也未重跑天翼下载门禁。
- 样本验收观察到 total/success 各 +1、历史 +1，failed/cached 不变，对应本次普通解析；其余验收不清零、不回扣统计。检查期间保留期/后台 session/网盘授权/env/seed 均未变；没有扫码、续期、转存、删除或其它云盘写入。
- 部署回执在 `/opt/jxpan-rust-test/releases/2f223a23f1ab/{result.json,verify-result.json}`；本机 ignored `.scratch/season1-8789/` 保存独立工具、制品、脱敏结果及 0600 私有响应。未 commit/push。

## 2026-10-14 解析并发与完整性发布（第一阶段，历史运行版本）

本轮先将 **8789** 从 `test-2f223a23f1ab` 更新到 `test-998b1c3825b2`，随后部署下节的最终空列表兼容版本。未替换 8788；各阶段的测试数、回执与实机窗口单独保留，不改写成最终结果，也不将测试发布称为迁移完成。

- 第一阶段镜像：`jxpan-rust:test-998b1c3825b2`。
- 实际 binary SHA-256：`998b1c3825b2e7d003014e330925a55e8927840143176d33dd6f7e0d7b4f99f8`；**8626680 字节，静态 Linux x86_64**。
- Cargo/src/web 内容指纹：`252286f8fb02a15400c1f49156ecc56a5bf727517a88bafff9bb23cb85e98698`；不是 Git tree hash。
- 该阶段 **269 项 Rust 测试全部通过**，fmt、严格 all-target clippy、**3086 前端 synthetic checks**、隔离 `verify-pages.py` 及 guarded deployer **22 项 synthetic 测试**通过。startup framing 回归已修复；不是完整浏览器或供应商全场景验收。
- `parse::run` 的 120 秒 timeout 只包围供应商工作；`finish` / 持久化在此预算外执行一次。synthetic SQLite actor stall 修复前为两条历史写入，修复后为一条，普通/fast 超时计账回归通过；不回扣已有数据。
- 受限 SQLite backup：`/opt/jxpan-rust-test/releases/998b1c3825b2/before-update.sqlite`，**0600**、`quick_check=ok`。切换瞬间**全部表内容相同**，history/stats/session/auth/retention/private env/seed 保留；没有 reset、后台登录、DB restore 或 seed 覆盖。8788 的容器 ID、镜像、启动时间和 worker 未变。
- 构建上下文严格为五项（静态 binary、Dockerfile、NOTICE 与指定许可证），不发送整个仓库或私有 env/seed/DB。guarded deployer 的 readiness 修复在切换前快照旧首页 hash，避免用新首页 hash 判断旧版本恢复；回滚验证只有 synthetic 测试，**未做线上回滚演练**。
- 该阶段移动同一授权样本：树 HTTP **200** / `tree_complete:true`；`Season 1` 两个 MP4 的名称/原始 ID/字节大小与完整供应商参考 JSON 相同；fid 子目录 **200**、两文件 metadata 相同；down **302、302**，已知目录 down **400**。单次 tree / 子目录 / 两条直链为 **1.330 / 5.634 / 0.612、0.478 秒**，不是性能基准；CDN 跟随、云盘写入、auth refresh、登录为 **0**。
- 该阶段移动主窗口 delta 为 **total +2 / success +1 / failed +1 / cached 0 / history +2**。脱敏只读分类确认仅一条 code 200 匹配选定分享，另一条 502 是**不同分享**的在线活动，不能归因于本次一个 parse。不 reset、扣减累计值或删记录；旧历史及全部 KV/session/auth/settings 保持，`rust_parse_settings` 行不存在，使用默认 **4**。
- 该阶段天翼同一冻结授权样本 HTTP **200** / JSON **code 502**，消息 `天翼目录列表获取失败`，未尝试下载。metadata 探针有 `fileId`、无 `shareId`；后续只读诊断分别确认 web 路线 `InvalidSessionKey` 和正确提取码的 v3 成功零总量响应，不以 metadata 形状判定整套授权过期。此处列表失败已由下节修复，成功下载门禁未因此关闭。
- 该阶段回执：`/opt/jxpan-rust-test/releases/998b1c3825b2/{result.json,verify-result.json}`；本机 ignored `.scratch/concurrency-8789/{release,apply-result,verify-result,live-result,cloud-live-result,accounting-projection}.json`。私有响应/凭据/数据库不提交。

## 2026-10-14 天翼显式零总量空列表兼容修复（历史运行版本）

该阶段仅更新 **8789** 为下列版本；并发/滚动调度与第一阶段其它修复保持，新增天翼成功零总量省略数组兼容，不恢复旧“缺列表一律成功”。本节“最终/当前”指该历史阶段，不是本轮最新结果；当前契约见 [PARSE_SETTINGS](PARSE_SETTINGS.md)。

- 该阶段镜像：`jxpan-rust:test-1807efaa0aed`。
- 实际 binary SHA-256：`1807efaa0aede75bd0d51626904f5795357ec3f7552b0d369381e053ac2e6985`；**8626744 字节，静态 Linux x86_64**，已只读复制运行 binary 核验，不只依赖镜像标签。
- Cargo/src/web 内容指纹：`a3549ac773e6ef46d7f710d389bb6274cf0c68c03b85fd9f157f732c9bf60467`；build context SHA-256：`f2ff150c85b15f11b0a4ac9e37325044067a0c1e368e2ad0f0ff02459897fc4c`，仍严格五项且无私有配置/数据。
- 最终 **270 项 Rust 测试全部通过**，fmt、严格 all-target clippy、**3086 前端 synthetic checks** 与隔离 `verify-pages.py` 通过；最终 guarded deployer **28 项 synthetic 测试**通过。真实 v3 响应形状的独立 loopback 回归先红后绿，另覆盖非零/缺失/矛盾总量、嵌套权威、非法 fileListSize、畸形数组及失败码；冻结 fixture 未改。
- 新 backup：`/opt/jxpan-rust-test/releases/1807efaa0aed/before-update.sqlite`，**0600**、`quick_check=ok`。最终切换瞬间**全部表内容相同**，stats/history/session/auth/retention/private env/seed 保留，无 reset、后台登录、DB restore 或 seed 覆盖。8788 容器 ID/镜像/启动时间/worker 未变。
- 上一阶段容器 `jxpan-rust-test-prev-998b1c3825b2` 已停止、`restart=no`。`prev-2f223a23f1ab`、`prev-db1165cf1d26`、`prev-2cd8947b306d`、`prev-533554636198` 同样保留；均只供**代码回滚**，共用测试 DB，**不得同时启动**，不借回滚恢复备份覆盖用户后续数据。未做线上回滚演练。
- 最终移动验收：普通树 **200** / `tree_complete:true`、fid 子目录 **200**；同两个 MP4 的名称/ID/字节大小精确匹配完整参考 JSON；down **302、302**，已知目录 down **400**。单次 tree / 子目录 / 两条直链为 **1.742 / 0.818 / 0.613、0.616 秒**，不是同环境加速基准。窗口 delta **total +1 / success +1 / failed 0 / cached 0 / history +1**。
- 最终天翼沿用同一冻结样本：HTTP **200** / JSON **code 200**，**目录列表成功但当前为空**，无可下载文件；download status 为 null，**未请求下载**。单次列表 1.741 秒，窗口 delta **total +1 / success +1 / failed 0 / cached 0 / history +1**；成功下载门禁仍未通过，不替换或隐藏失败样本。
- 两项最终检查均保留旧历史、全部 KV、retention、session、授权、设置与 private env/seed；`rust_parse_settings` 行仍不存在，使用默认 **4**，未为验收保存设置。运行无 OOM/重启；CDN 跟随、账号登录/续期、云盘写入、DB restore/reset 均为 **0**。302 只证明返回直链，不证明播放或文件正文下载。
- 目标机回执：`/opt/jxpan-rust-test/releases/1807efaa0aed/{result.json,verify-result.json}`；本机 ignored `.scratch/concurrency-empty-8789/{release,apply-result,verify-result,live-result,cloud-live-result,acceptance-summary}.json`。最终 `acceptance.py` **退出 1** 仅因天翼缺少下载正例，摘要中移动为 true、天翼下载 gate 为 false；不是本地测试、部署或天翼列表失败，也不能写成全部实机通过。本轮没有 stage/commit/push。

该历史阶段尚缺真实 >200 分页/cursor、天翼 password/action 路线、真实扫码/续期、CDN 播放与同环境 RSS/CPU/速度；其中移动分页/短期并发基准由下节本轮结果更新，不能继续作为当前完全未验证项。授权刷新/QR 不在公开分享并发预算内，记录只有 retention 而无总 quota，管理员 session 未绑定当前配置版本、旧 DB 权限不强改，仍保留现有边界。

## 2026-10-14 分页计数、提取码与默认并发发布（历史运行版本）

本轮主代理仅将 **8789** 从 `test-1807efaa0aed` 更新为 `jxpan-rust:test-e897dd1e919a`，没有替换 8788。本节基于既有脱敏回执整理，不重新执行网络、SSH 或部署。

- 实际 binary SHA-256：`e897dd1e919a7015d7a49b8e85ee988da7e177c85e2c00987b54a737f7506c0f`；**8625784 字节，静态 Linux x86_64**。source（Cargo/src/web）内容指纹：`53e00a31c76ac839d973e6de649c0bfb11da5cbdc4171c0ca4091223579412f3`；build context SHA-256：`4fc4f3f52d70cf963c68d6fa8f9274be26b7295d63b477d5d8e5a0b708747b04`。未提交工作区构建，不只凭 Git 基点/镜像标签断言制品一致；context 仍只有静态 binary、Dockerfile、NOTICE 与指定两份许可证，无 private env/seed/DB。
- 已由主代理完成并验证：**279 项 Rust 全套**、fmt、严格 all-target clippy `-D warnings`、**3086 前端 synthetic checks**、隔离 `verify-pages.py`、**35 项 deployer + 14 项 orchestration 测试**。这些不等于完整浏览器、全供应商或线上回滚演练。
- 移动 `caLength/coLength` 修正为本页计数并严格校验实际数组；任一列表满 200 或 active opaque cursor 就继续，仅短页无 cursor 停止，满 200 需要尾页；`nodNum` 不用于分页。安全预算、循环/重复检测不放宽。天翼 sid + 非空 pwd 不再绕过密码重验，fresh client 捕获密码响应 cookie 并传给 AppShared/OpenShared；公共下载仍禁止 owned/转存/删除，不探测 Range、不拉正文。
- 代码默认并发改为 **16**，范围仍 **1–16**，已保存合法值优先、缺键初始化不写值。最终只读审计确认已保存并发 **4**，按已验证构造逻辑推断当前有效值为 **4**（未直接观察授权设置 endpoint）；未覆盖保存值，也未登录或替换 session。审计窗口保护状态、stats/history 和已有服务不变。默认选择与完整限制见 [CONCURRENCY_BENCHMARK](CONCURRENCY_BENCHMARK.md)。
- 切换验证：实际运行 binary、页面/资源/私有权限均通过，SQLite **`quick_check=ok`**，**切换全部表内容一致**；settings/session/auth/stats/history/retention/private env/seed 保留。未 reset、restore、后台登录或替换 session。8788 的身份/镜像/启动时间/worker 不变。
- 上一版本容器 `jxpan-rust-test-prev-1807efaa0aed` 已停止、**restart=no**，旧回滚容器/镜像留存。它们只供代码回滚且共用测试库，**不得同时启动**，不得用旧备份覆盖切换后数据；没有线上回滚演练。
- 最终真实移动列表 **HTTP 200 / JSON code 200 / `tree_complete:true`**：**7 目录（含 root）、2260 唯一文件、最大直接文件目录 501**。旧生产路径出现 `mobile_pagination`，修复后此单样本真实 >200 分页通过；**15.124 秒**为单次验收，不是并发性能基准，也不证明所有 cursor/分享场景。
- 最终真实天翼列表 **HTTP 200 / JSON code 200**，**76 唯一文件**。另一次只读 diagnostic 密码校验成功、列表 76，但没有收到分享 cookie；AppShared **HTTP 400 InvalidSessionKey**，OpenShared **HTTP 400** 拒绝且无下载 URL。代码密码/cookie 回归不代表真实下载修好；**天翼成功下载门禁仍未通过**，未登录/刷新/替换 session 或使用 owned 回退。
- 最终验收窗口只有两个普通 JSON 列表请求：**total +2 / success +2 / failed 0 / cached 0 / history +2**，旧历史、全部 KV/session/auth/设置、private seed 保持；累计统计未下降。列表验收的下载 URL 请求和 CDN 为 **0**；独立下载地址 diagnostic 不拉正文、不跟随 CDN，保护状态保持、stats/history delta 为 0。两者不能混写为已成功下载或“整个本轮没有地址诊断”。
- 并发证据另列：旧代码冷池两样本各 22 次、新代码 shared 池各 12 次，共 **68 次干净 benchmark walk**（不计 inventory）；fingerprint 完整一致、峰值达到候选，保护状态/stats/history 无变化。另一个旧 shared 批次虽 24 次 walk 成功，但窗口有外部活动，排除默认决策，不归因 probe。不用单次大分享的 15.124 秒代替并发基准。
- 回执：本机 ignored `.scratch/concurrency-fix-8789/{release,apply-result,verify-result,real-listing-acceptance,mv02-shared-fixed,mv03-shared-fixed,saved-settings-audit,final-report}.json`；下载诊断 `.scratch/cloud-download-8789/diagnostic-v5-result.json`；冷池 `.scratch/concurrency-bench-8789/{mv02-cold-01,mv03-cold-01}.json`。不发布分享 URL、密码、原始 ID、下载 URL 或凭据。

尚缺：天翼成功下载/可测宽目录/真实大分页、完整移动 cursor/分享矩阵、真人扫码/续期、CDN、CPU/RSS、长期 p95/错误率与线上迁移/回滚演练。当前保存并发 4 的只读审计已完成，有效值 4 是依据已验证构造逻辑的推断，不是授权 endpoint 的直接观测。默认 16 是当前移动样本七个已测候选中的暂定性能选择，不是所有整数/所有供应商最优、官方上限或长期稳定保证；不能据此替换 8788。

## 首次测试部署的隔离与凭据（历史约束）

- 新建独立 `data/jxpan.sqlite`，不挂载、导入或修改旧数据库；解析历史默认 **7 天**。
- 后台账号密码只在目标机内部沿用旧服务的显式配置，没有默认密码，也不在报告中输出。
- 只读提取旧服务的天翼/移动两条授权记录，作为私有启动快照；不导出完整数据库。配置存在不等于在线授权验证通过。
- 初次部署当时设置 **`JXPAN_AUTH_REFRESH=false`**，避免共享授权自动刷新。**当前源码已移除该开关，该环境值不再生效**；受管账号解析/取址可能按需续期，只有初始化/状态读取等本地只读路径不请求供应商。后台显式扫码/修改不回写旧部署数据库，但供应商侧账号会话影响仍需谨慎；真人扫码和真实长周期续期未验收。
- 私有配置与构建过程保存在 ignored scratch / 目标机受限目录；不放进 runtime 镜像或 Git。不要打印 `docker inspect` 的完整环境变量。
- 非 root UID 10001、只读根文件系统、去除 capabilities、no-new-privileges；256 MiB 内存、2 CPU、64 PID 上限；Docker 日志单文件 5 MB、最多两份。
- `unless-stopped` 重启策略；仅绑定用户指定主机地址的 8789。公网、反向代理或其它 origin 未配置，也未新增防火墙规则。

`deploy/Dockerfile` 的 build context 只能含静态 `jxpan-rust`、Dockerfile、NOTICE 和指定许可证，**不能将整个仓库作为上下文**。首次 8789 测试部署拒绝已有容器名/目录，后续升级先确认目标、备份及代码回退边界；这是历史测试流程，当前 8788 正式接管已验收，状态与独立验收边界见本页上方。

## 首次部署后实际验证（历史，不代表本轮重新登录）

- 远程可访问首页/登录页；实际 Chrome 首页加载及安全授权元数据展示通过。
- 首页、登录、授权后台和本地 QR 资源 hash 与当时工作区一致。
- 使用目标机内部配置完成真实 HTTP 后台登录、私有响应头、7 天设置读取、退出及 token 失效；没有输出会话或密码。
- 测试库 `PRAGMA quick_check` 为 `ok`。
- 旧服务容器 ID、镜像、启动时间及 worker SHA 未变，旧首页仍 HTTP 200。
- 没有执行供应商 QR、续期、转存、删除或其它云盘写入；天翼成功下载门禁仍未通过，见 [STATUS](STATUS.md)。

## 历史测试实例的停止边界

早期独立测试实例名为 `jxpan-rust-test`，停止它当时不影响旧 `jxpan` / 8788。正式接管后不能继续按这个历史名称或端口关系猜测当前服务；先核验当前运行身份、维护脚本与精确范围，再执行获授权的操作。不执行全局 prune，不删除私有数据库/备份或波及其它项目。后台 cookie 的域不按端口隔离；两页面使用同名 cookie 时交替登录可能需要重新登录相应后台，这不表示数据库共享。
