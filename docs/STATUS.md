# 当前实现与验证状态

> **8788 已正式接管，部署与精确清理已核验（2026-10-15）。** apply、独立 verify 及独立 final audit 均通过；Git 提交推送按独立步骤核验，结果以 Git 历史/回执为准。旧 8789、8788 未动及回滚容器留存记录只适用于各自历史窗口。

## 当前正式部署（2026-10-15）

- 已验证的 8789 最终 binary SHA-256：`f7496a9c10e0f5c576b9b6f9d5f759d6a20d3658709d441bf629c47babd4f7ed`；source 内容指纹：`dfa14505f2c7a660e351bb6dc8d618a9fdacffdd3267549e292d3e423a57110d`。制品身份以部署及独立审计回执核验；旧 Git 基点 `25b97f253266e6aa497fef763ab1ac05271ba062` 不足以重建包含构建时工作区变更的同一制品。
- 已核验 **351 项 Rust、6698 项前端 synthetic checks、44 项部署工具测试、9 项 verifier safety 测试、fmt 及 Homebrew Rust 1.93.1 严格 Clippy**；最终再次运行 Rust/前端/隔离 verify-pages/9 项 verifier safety/fmt/严格 Clippy 全部通过。这些不等于完整浏览器/供应商矩阵验收；更早的 288/3086 等数字保留在各自历史窗口。
- 后台统一“网盘账号”卡片与详情，仅展示白名单安全元数据，无原始凭据或调试信息。初始化、状态读取、首页/匿名后台检查对供应商网络请求为 **0**；解析/取址对已登录且启用的受管账号强制按需尝试续期。源码无 `JXPAN_AUTH_REFRESH` 开关，无后台定时保活，移动禁用状态不因续期开启而解除。
- 天翼 S005/S006 目录内文件取址由 400 恢复为 **302**，已有取址正例；不能再概括为天翼完全不能取址。302 不代表 CDN 正文、播放、目录 ZIP 或完整下载矩阵通过。真实长周期续期、真人扫码仍未验收。
- 支持范围仅为**天翼 + 移动**；其它网盘仍明确 `success:false/code:501`。公共天翼仅 Open/AppShared，不允许 owned 私有文件回退、转存或删除。
- 正式容器 `jxpan-rust`：`6b84cd5b2d61e31dfcd4d8e6221d838b162b21b076a793d1e99a655617cf5ed6`，StartedAt `2026-10-15T08:38:01.181710027Z`（16:38:01 +08:00）；不可变 image ID `sha256:03cc3c14fe8a2c07e813eb82d321e5ad57adc260cc30777881be692b5f899d71`，唯一项目镜像标签 `jxpan-rust:release-f7496a9c10e0`。外部 `172.18.77.1:8788` → 内部 8789，`PUBLIC_ORIGIN` 对齐外部 8788；正式入口、实际运行制品、页面/权限已独立验证。
- `/opt/jxpan-rust-test` 原子 rename 为 `/opt/jxpan-rust`，真实 DB inode **18885230** 保留，交接全部表/保护 KV 一致；新鲜只读备份未恢复，运行 `quick_check=ok`，stats `[60,45,15,13]` 未下降，当前 accounts/settings/session 保留。旧 Miniflare 数据仅归档，不合并旧 DB，原 `/opt/jxpan` 未改。
- 已精确移除 14 个旧项目容器、11 个旧项目 image ID 及 12 个旧 tag（含当前测试别名）；独立 final audit `success:true`，旧容器/tag 删除回执齐全，唯一项目 release image 已核验。9 个无关容器的完整 Config/HostConfig/Mounts/运行状态、其它带标签镜像保持。另观察到 110 个无标签、约 8.5–8.8 MB 中间镜像消失；结合 scoped `rmi` 推断为 Docker 默认祖先自动删除，不宣称全部 images 未变。原 cleanup 末尾库存断言失败回执保留，不改写。独立审计同时通过正式身份/StartedAt/实际 binary、全配置挂载、端口、DB inode/权限/env、备份 tar/SQLite 与 legacy 树保持、旧测试入口 8789 拒绝连接、正式首页/登录/QR hash 和匿名 401 核验。
- 临时 context 包精确清理已完成：本地 8 个共 **69,140,480 B**、远端 10 个共 **86,446,080 B**，只删文件未删目录。保留 latest UI/直接前版 auth-refresh 及远端当前 release context、全部脚本与 attempt/result、工具链/缓存、凭据/private、DB/备份与 legacy 归档；旧包缺失意味着历史脚本不构成完整回退。审计回执、精确 manifest/result 与维护工具用法见 [TEST_DEPLOYMENT](TEST_DEPLOYMENT.md)。

下方有日期的旧部署、保存并发 4/16、下载失败和禁用刷新记录只描述各自历史窗口，不覆盖本节当前契约。

## 冻结基准与页面来源

- 迁移前从旧部署容器只读冻结 worker，而非以上游最新版代替部署版。
- worker SHA-256：`47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118`；冻结/历史验证时通过只读 SSH 确认一致。
- 原源码、账号凭据、分享/下载 URL、私有测试输出只放 `.scratch/baseline/`，不提交。
- 原首页/登录页的冻结 SHA 与历史旧部署 GET 内容一致；后台源自相同 worker 的 `admin(true)`。当前正式入口已是 Rust，不再用其页面充当旧 worker hash 基准。
- 当前 `web/index.html` / `web/admin.html` 有明确的授权、日志、XSS、搜索/移动下载及本地 QR 补丁，**不再与旧页面逐字相等**。登录页未改；原始 manifest 不重写。差异见 [FRONTEND_CHANGES](FRONTEND_CHANGES.md)。
- `verify-pages.py` 分开验证旧部署原始 hash 和 Rust 当前补丁页面：本地匿名/授权后台页面、会话 cookie、私有头、QR 静态资源及 HEAD/OPTIONS 已通过。旧部署验证仅 GET，不登录或写入其后台。

## 已实现及验证边界

| 组件 | 当前实现 / 验证证据 | 仍有边界 |
| --- | --- | --- |
| 页面 / HTTP 兼容 | GET/POST/HEAD/OPTIONS、参数与普通 multipart 接线；统一网盘账号 UI、元数据白名单；64 请求准入、body 15 秒 / handler 150 秒 / 供应商解析 120 秒边界 | 没有完整浏览器/真人扫码验收；其它网盘仍为 501；parse 收尾/持久化不在供应商 120 秒预算内 |
| 天翼协议及只读解析 | metadata/shareMode、目录 size 占位、公开分享无关提取码、Open 固定 32 位 MD5及已知错误分类修复；历史 40 分享/44 样例，19 个不同完整目录树，最大 3357 唯一文件；后续 S005/S006 目录内文件取址 302 | 旧窗口 Open/App 认证拒绝不代表当前全部取址失败；CDN 正文/播放、目录 ZIP 和完整认证/下载矩阵未验收；禁止 owned/转存；同目录最多实测 50 项，真实 >200 分页仍缺 |
| 移动协议及只读解析 | caLength/coLength 按本页计数严格校验实际数组；满页/active opaque cursor 继续；真实大分页 HTTP/JSON 200、完整树 7 目录（含根）/2260 唯一文件，最大直接文件目录 501 | 单样本不覆盖所有 cursor/分享场景；历史 302 不证明 CDN 播放；真实扫码/续期矩阵未完成 |
| 原始目录索引 / 完整共享树 | 生产 walk_complete 滚动调度、固定本次并发快照；失败/循环/全局重复 ID 拒绝；重名不覆盖、未知 size=null、0 有效、大值字符串 | 10k 目录 / 20k entries / 8 MiB 估算 / 96 层，任一超限即错误；冷缓存 fast 仍不能宣称所有目录已识别 |
| 解析并发设置 | 后台 get_parse_settings / set_parse_settings，整数 1–16、代码默认 16；已有合法值优先，缺键不写 KV；10-14 保存 4 / 10-15 保存 16 是独立历史只读观察 | 最新生产有效值须由正式验收确认，不把历史保存值当作本轮新观测；默认选择仅依据移动样本，不代表天翼或全场景最优；auth/QR 不在公开分享预算内 |
| 有界缓存 / 请求合并 | 取消、失效、淘汰、计账、panic、代际隔离；Hit/Joined/Loaded 来源；失败不留 ready cache | 解析 8 MiB、下载/目录各 256 KiB 是保留值预算，不是 RSS 上限 |
| 动态授权 / 二维码 / 续期 | lib / AppState / router / SQLite 接线；受管账号解析/取址强制按需续期、明确拒绝最多一次重试；状态读取不请求供应商；移动禁用仍禁止解析及续期 | 未完成真人扫码/真实长周期续期；配置存在不等于在线有效；无关闭续期开关或后台定时保活；刷新/QR 无统一分享 I/O 并发上限 |
| 后台登录 / 权限 | 无默认凭据、持久化 token 摘要、登录限速、7 天 cookie、退出、同源 POST、私有响应头 | session 未绑定当前管理员配置版本；本轮不失效活动 session，不强改旧 DB 权限；代理须明确 public origin |
| 统计 / 记录 / retention | 分类、计数、分页/搜索、删除、持久化/回滚；payload 在 SQL 复制前检查字节 | 默认 7 天，仅 retention 无总行数/磁盘 quota；只清 Rust parse_records，累计统计不随删除下降 |
| KV / 短链 | KV 明文 2 MiB、密文上限与 SQL 前字节检查；短链 10k 拒新增、相同规范目标复用、不淘汰旧链接 | KV 没有总磁盘 quota；短链 expires_at 不新增 TTL；记录 retention 不清短链；兼容 AES 不是真实秘密保护 |
| 转存安全内核 / 自有目录能力 | synthetic 测试、副作用模拟、分页、隔离 scope、恢复回执、默认关闭 | 未接公共解析；OAuth 与 QR/session 同账户绑定未验收；真实创建/转存/删除未验收 |

组件依据见 [PARSE_SETTINGS](PARSE_SETTINGS.md)、[CONCURRENCY_BENCHMARK](CONCURRENCY_BENCHMARK.md)、[AUTH](AUTH.md)、[RECORDS](RECORDS.md)、[SHORTLINKS](SHORTLINKS.md)、[CLOUD189_TRANSFER](CLOUD189_TRANSFER.md)、[CLOUD189_TRANSFER_CAPABILITIES](CLOUD189_TRANSFER_CAPABILITIES.md)。

## 历史天翼分享兼容、认证分类与实测（2026-10-15，test-3d1cee097916）

该窗口只更新 8789、8788 未动；288 项 Rust、fmt、严格 Clippy 1.93、3086 前端 checks、隔离页面、48 项 deployer 与 32 项 probe-runner 测试通过。当时 40 个真实分享/44 样例有 19 个不同完整目录分享、最大 432 目录/3357 唯一文件，未取得下载地址；这是当时的认证阻塞，不覆盖后续 S005/S006 的 302 正例。最终只读审计保存并发 16、全部表与切换前相同、stats/history delta 0；未写设置或替换登录。

- 运行时代码仅 `cloud189_client.rs`、`cloud189_service.rs`、`http/reply.rs` 相对上一部署改变。Open 下载同参数/时间戳对照：31 位签名为 InvalidSignature，完整 32 位为 AccessTokenHasExpired；通过签名检查不等于下载成功。
- PanSou 九组关键词有 4029 条去重前结果；生产 crate 隔离探针执行 40 个不同分享、44 个样例（含 4 个密码变体）。parse code：200×20（19 个不同目录）、401×7、404×14、403×2、400×1。20 次目录后的 fresh-client fast 均认证 401，下载地址为 0，CDN 为 0；不能将没有笼统 502 当成下载成功。
- 最大完整树 432 目录（含根）/3357 唯一文件，约 59.756 秒；同目录最大直接文件数 50，因此没有真实 >200 同级分页证据。探针固定并发 4，不写回配置；最终只读观察线上已保存 16，切换及探针窗口没有设置/session/auth 或统计/历史改变。
- 新服务按不可变 image ID 启动；实际 binary、页面、权限、DB quick_check 与全部表内容保持均核验通过。上一测试容器停止且 restart=no，原 8788 身份/worker 保持，未 stage/commit/push。
- 追加 Rust 1.94.1 工具链核对：编译/288 测试通过，但严格 Clippy 1.94 在未改的 `src/http/mod.rs:75` 触发 result_large_err；1.93 严格 Clippy 通过。没有无关 source 改动或加 allow 隐藏诊断，全工具链检查并非全绿，见 `local-verification.json`。
- 证据、指纹、错误分类边界及安全运行器诊断见 [CLOUD189_SHARE_REGRESSION](CLOUD189_SHARE_REGRESSION.md)。该窗口的下一步是用户重新登录后补取址正例；后续已有本页顶部记录的 S005/S006 302，不能继续把这条历史阻塞当成当前完全未成功取址。

## 历史分页、提取码、并发基准与发布（2026-10-14，test-e897dd1e919a）

- 移动旧生产路径在真实大分享上出现 `mobile_pagination`；`caLength` / `coLength` 实为各列表本页计数，不是累计 total。当前严格核对本页实际数组长度，不用 `nodNum` 分页；任一 ca/co 列表满 200 或存在 active opaque cursor 就继续，仅短页且无 cursor 停止，满 200 仍需尾页。分页/响应/整树预算和重复检测没有放宽。
- 天翼 fast 下载即使传入 sid，非空 pwd 仍须重验；fresh client 捕获密码响应 cookie 并用于 AppShared/OpenShared。真实 diagnostic 密码通过且列表 76 文件，但该响应没有分享 cookie；AppShared HTTP 400 `InvalidSessionKey`，OpenShared HTTP 400 拒绝且无下载 URL。因此代码回归已修，不代表真实下载已修好；未登录、刷新或替换 session。
- 两个移动宽目录样本的旧代码冷池各 22 次、新修复代码 shared 池各 12 次，共 **68 次干净 benchmark walk**（不计 inventory）；全部完整 fingerprint 一致，峰值完整 list future 达到候选，保护状态保持、stats/history delta 为 0。旧 shared 批次虽 24 次 walk 成功，但窗口有外部活动，排除默认决策、不归因 probe。方法、各档中位数与限制见 [CONCURRENCY_BENCHMARK](CONCURRENCY_BENCHMARK.md)。
- 代码默认改为 **16**、允许范围仍 **1–16**；已有合法值优先，缺键初始化不写默认值。最终只读审计确认当前保存并发 **4**，按已验证构造逻辑推断当前有效值为 **4**（未直接观察授权设置 endpoint）；设置/session/auth、stats/history 和已有服务保持，没有 settings write 或登录替换。默认 16 不覆盖保存值，不能把基准候选 16 写成线上已有效 16。
- 主代理已验证 **279 项 Rust 全套**、`cargo fmt --check`、严格 all-target clippy `-D warnings`、**3086 前端 synthetic checks**、隔离 `verify-pages.py`、**35 项 deployer + 14 项 orchestration** 测试通过。这些层级不替代真实供应商或完整浏览器验收。
- 仅发布 8789 镜像 `jxpan-rust:test-e897dd1e919a`；实际 binary SHA-256 `e897dd1e919a7015d7a49b8e85ee988da7e177c85e2c00987b54a737f7506c0f`（8625784 字节），source 指纹 `53e00a31c76ac839d973e6de649c0bfb11da5cbdc4171c0ca4091223579412f3`，context 指纹 `4fc4f3f52d70cf963c68d6fa8f9274be26b7295d63b477d5d8e5a0b708747b04`。实际运行 binary、页面/安全权限、`quick_check=ok` 与切换全部表内容一致通过；prev-1807efaa0aed 停止且 restart=no 留存，8788 不变，无 reset/restore/登录/session 替换。详见 [TEST_DEPLOYMENT](TEST_DEPLOYMENT.md)。
- 新版本真实移动普通列表 HTTP 200 / JSON code 200 / `tree_complete:true`，**7 目录（含 root）、2260 唯一文件、最大直接文件目录 501**；单次 **15.124 秒**仅为实机验收，不是基准。天翼 HTTP/JSON 200、**76 唯一文件**；该最终窗口仅两个普通列表请求，total/success 各 +2、history +2、failed/cached 0，旧历史和设置/session/auth 保留，没有下载 URL 请求、CDN、登录、续期或云盘写入。
- 脱敏证据：`.scratch/concurrency-fix-8789/{release,apply-result,verify-result,real-listing-acceptance,mv02-shared-fixed,mv03-shared-fixed,saved-settings-audit,final-report}.json`，`.scratch/cloud-download-8789/diagnostic-v5-result.json`。天翼下载、完整 cursor/供应商矩阵、长期稳定/p95、CPU/RSS、CDN 与线上回滚演练仍有缺口，不宣称全供应商最优。

## 历史解析并发发布与验证（2026-10-14，test-1807efaa0aed）

以下保留该阶段的测试数、设置缺键/默认 4、两文件/空目录及部署回执。其移动 ca/co 累计 total 解释与“真实 >200 未验证”的边界已由上节本轮修复/验收更新，不能照作当前契约。

- 两网盘列表每页 200、最多 100 页；单响应 2 MiB，单目录累计 4 MiB 估算 / 20k items。天翼成功路线固定分页；移动 ca/co 分别核对 total，不用 nodNum，opaque nextPageCursor 原样传递。失败、页重叠、循环、全局重复 ID、资源超限都不以部分树成功兜底。
- 原天翼 folder-item missing-idMap 断链已纠正：根据真实 Walk 在请求子树内查文件/目录；不存在 404，目录输出子树，文件只用公开下载方法。parse_file 不信 caller 的 name/size，使用上游 metadata；合法 0 字节不是目录，直链不追加 filename。
- 公共天翼只允许 Open / AppShared。Range 一跳探测已删除，直接供应商 URL 302，服务端不拉文件/视频正文。该差异不等于真实天翼下载或 CDN 播放成功。
- startup framing 丢失 size hint 的回归已修复；此前 265 通过 / 2 失败、第一阶段 269 通过均是中间结果，不是最终结果。最终 **270 项 Rust 测试全部通过**，`cargo fmt --check`、严格 all-target clippy、**3086 前端 synthetic checks**、隔离 `verify-pages.py` 与最终 guarded deployer **28 项 synthetic 测试**均通过；合成验证不等于完整浏览器、供应商全场景或线上回滚演练。
- `parse::run` 的 120 秒 timeout 仅包围供应商工作，`finish` / 持久化在其外执行一次。synthetic SQLite actor stall 在修复前复现两条历史写入，修复后为一条；普通/fast 超时计账回归通过。这是收尾重复写入的修复，不清零、不回扣既有统计。
- 天翼真实 v3 成功响应 `res_code=0`、`fileListAO:{count:0,fileListSize:0}` 可省略两组空数组。新增独立回归先红后绿：嵌套 count/total 保持权威，先核对总量；仅明确总量 0 可在缺数组时成功，非空/缺失/冲突总量、非零或非法 fileListSize、畸形数组、失败码仍拒绝，不恢复旧“缺列表一律空成功”。冻结 fixture 未改。
- 仅更新 8789 为 `jxpan-rust:test-1807efaa0aed`；实际静态 Linux x86_64 binary SHA-256 为 `1807efaa0aede75bd0d51626904f5795357ec3f7552b0d369381e053ac2e6985`（8626744 字节），Cargo/src/web 指纹为 `a3549ac773e6ef46d7f710d389bb6274cf0c68c03b85fd9f157f732c9bf60467`。SQLite `quick_check=ok`，切换瞬间全部表内容相同；统计/历史/session/授权/retention/private env/seed 保留，无 reset、登录替换或 DB restore。8788 的容器 ID、镜像、启动时间、worker 均未变；部署保护与代码回滚边界见 [TEST_DEPLOYMENT](TEST_DEPLOYMENT.md)。
- 最终版本移动同一授权样本：普通树 HTTP 200，`tree_complete:true`；`Season 1` 两个 MP4 的名称/ID/字节大小与完整供应商参考 JSON 相同。fid 子目录 HTTP 200 且两文件 metadata 相同；两条直链各 302，已知目录的直链请求 400。没有跟随 CDN，没有云盘写入、auth refresh 或登录。单次 tree / 子目录 / 两条直链耗时分别为 1.742 / 0.818 / 0.613、0.616 秒，只是样本，不是加速基准。
- 最终移动验收窗口累计 delta：total +1、success +1、failed/cached 0、history +1；旧历史及全部 KV/session/授权/设置未变。`rust_parse_settings` 行仍不存在，使用默认 4，不为验收写入设置。第一阶段窗口夹杂不同分享的在线 502，已只读分类且未重置/扣减数据；该阶段记录保留在 [TEST_DEPLOYMENT](TEST_DEPLOYMENT.md)，不混入最终窗口。
- 天翼同一冻结授权样本最终为 HTTP 200 / JSON code 200，成功返回空目录；没有可下载文件，**未请求下载，成功下载门禁仍未通过**。该窗口 total/success 各 +1、history +1、failed/cached 0；保护状态保持。此前 web 路线明确返回 `InvalidSessionKey`，正确提取码的 v3 路线则成功返回显式零总量；metadata 探针有 `fileId`、无 `shareId`，不据此断言整套授权过期。没有替换样本、账号登录/续期或云盘写入。
- 最终脱敏回执位于 ignored `.scratch/concurrency-empty-8789/{release,apply-result,verify-result,live-result,cloud-live-result,acceptance-summary}.json`；私有样本不入 Git。`acceptance.py` 退出 1 对应天翼缺少下载正例，不是列表/部署/本地测试失败，也不是全部实机通过。真实 >200 分页/cursor、天翼 password/action 路线、CDN 播放及同环境 RSS/CPU/速度仍无证明。

## 历史收尾验证（不是本轮新实现或整套完成验收）

```sh
cargo fmt --check
cargo check --locked -j 2
cargo clippy --locked --all-targets -j 2 -- -D warnings
JXPAN_LIVE_ORACLE=1 cargo test --locked --no-fail-fast -j 2
node tools/verify-frontend.mjs
cargo build --locked -j 2
python3 tools/verify-pages.py --baseline <明确授权的现有服务地址>
python3 tools/verify-live.py --baseline <明确授权的现有服务地址>
```

- 全套 Rust：2026-10-14 移动单层响应/MP4 空目录回归加入后 **177 项通过**，fmt 和严格 clippy 通过；测试项数不是业务功能数。`JXPAN_LIVE_ORACLE=1` 仅重跑本地冻结 Node VM，并核验原 fixture，不调用网络云盘；本次普通离线测试、真实移动分享目录与 302 分开核验，未重跑天翼下载门禁。
- 前端：初次收尾 2831 checks；2026-10-14 补上按钮静态/注册双事件与单请求回归后 **2888 checks 通过**，仅 synthetic DOM/VM/QR 编码器；这不是完整浏览器扫码测试。
- 本地真实 Chrome smoke：原生登录、后台/记录页、默认 7 天、UI 保存为 8 天并重载持久化、同源本地 QR GIF 生成通过；未请求供应商二维码或扫码。退出按钮打开确认框，但当前浏览器工具不能处理 dialog，未完成该 UI 路径；loopback 退出/会话失效回归已通过。
- 页面：原部署两个公开页面 hash、原冻结源码/manifest，以及隔离 Rust 当前页面/后台 cookie/私有头/本地 QR/HEAD/OPTIONS/公开聚合统计全部通过。
- 移动：目录 JSON 相等；旧/Rust 下载均 **302**；CDN HEAD 均 **403**，GET Range 均 **206**，每条只读 **1 字节**。
- 天翼：目录 JSON 相等；旧/Rust 下载均 **502**。成功下载要求仍失败，未忽略或替换样本。
- `verify-live.py`：**2 条中 1 通过、1 失败，退出 1**；不是整套 live 通过。现网 SHA 与冻结来源仍相同，不能将这个失败归咎于已证实的版本漂移。
- 该历史窗口的本地验证使用临时 DB/工作目录、干净环境及当时仍生效的禁刷新选项，并正常回收子进程。**当前源码已移除该选项**，不能用 `JXPAN_AUTH_REFRESH=false` 隔离真实账号；当前安全隔离依靠无真实账号或内存 transport。SIGTERM 测试验证正常退出、DB 可重开/完整性和已提交记录保留，不宣称已验证所有进行中的网络请求排空。
- 真实 JSON 的时变签名只在明确的 `data.download_url` / `data.files[*].url` 路径规范化；下载行为独立检查，不以忽略 URL 当作成功。

## 2026-10-14 测试实例热修

用户实机反馈一次点击出现两次请求：原首页按钮 inline onclick 与注册 click listener 重复执行。仅移除 inline handler，增加先红后绿的单点击/主动重试/URL 自动解析回归，保留后端统计语义。175 Rust、严格 clippy、2888 前端 checks 和隔离页面验证通过；用户授权后更新 8789，真实 Chrome 单次点击确认仅一个被拦截的 synthetic parse 调用，未向测试后台发送诊断解析。

升级前正常停止测试实例并备份/核验 SQLite；切换前后全部表内容指纹相等，统计、记录、保留期、后台 session 与网盘授权未变。旧测试容器停止保留用于代码回滚，8788 容器/源码未变。已存在的误计次数不自动回扣，浏览器需刷新加载新 HTML；详见 [FRONTEND_CHANGES](FRONTEND_CHANGES.md) / [TEST_DEPLOYMENT](TEST_DEPLOYMENT.md)。本轮没有重跑真实下载门禁，不将该热修视为天翼下载验收通过。

## 2026-10-14 直链缓存对齐

用户授权将 `download_cache` TTL 从 60 改为 **600 秒**，对齐原版 play 缓存；只更新 8789 为 `jxpan-rust:test-db1165cf1d26`。容量、认证刷新和账号变更失效策略、解析/目录 TTL、日志保留期与前端均未改。新增真实 AppState 配置回归，确定性验证超过 60 秒仍保留、600 秒精确失效且读取不续期；176 Rust、严格 clippy、2888 前端 checks、页面验证通过。

切换前正常停止并备份 SQLite，全部表内容保持，统计/历史/设置/后台 session/网盘授权及私有环境保留；实际运行 binary、页面/权限、数据库完整性与 8788 未变已核验。旧测试容器停止保留，不恢复数据库、不替换用户登录。应用十分钟 TTL **不代表验证了上游签名十分钟有效**，也没有新增供应商请求。详见 [CACHE](CACHE.md) / [TEST_DEPLOYMENT](TEST_DEPLOYMENT.md)。

## 2026-10-14 移动分享空目录修复

用户提供的 `Season 1` 应有两个 MP4，升级前子目录 action 404 已复现。真实供应商响应是单层 CBC/PKCS#7：原版无条件再次裁剪，将完整 JSON 的末尾 `}` 当成填充长度而误删 125 字节；带 `]` 的文件名又令旧正则救援失败。Rust 继承了这个缺陷，现生产响应优先保留去标准填充后的完整 JSON，否则仍用原双层解码/救援；原请求体、冻结纯函数/旧响应向量、遍历和前端保持，新增完整 loopback 树/下载地址回归先红后绿。

177 Rust、严格 clippy、2888 前端合成 checks 和隔离页面验证通过。仅更新 8789 为 `jxpan-rust:test-2f223a23f1ab`；实际 binary、数据库备份/完整性、切换全部表内容保持、页面/权限及 8788 未变核验通过。用同一真实分享验收，正确返回两个 MP4，名称/ID/字节大小与完整上游 JSON 相同，两条直链均 302；没有跟随到 CDN，不冒充视频下载或天翼门禁通过。普通解析观察到 total/success 各 +1、历史 +1；其余保护状态保持，不恢复/重置数据。600 秒 TTL、retention、后台 session、授权、env/seed 均保留，详见 [MCLOUD](MCLOUD.md) / [TEST_DEPLOYMENT](TEST_DEPLOYMENT.md)。

## 默认七天日志的实际范围

- 后台 `get_log_settings` / `set_log_settings` 已接入，整数范围 **1–3650**；设置持久化，缩短时立即清理，增大不能恢复已删除记录。
- 启动、插入记录、修改设置和每小时 maintenance 清理严格早于截止时间的 Rust `parse_records`。
- 保留期和累计请求统计独立；ready-cache 命中计一次缓存成功，合并请求按实际成功/失败计数且不重复写历史。
- 不清理 KV、二维码、短链、旧部署数据库、Docker/系统日志，也不删除任何云盘文件。

## 明确差异及未完成项

1. **公共天翼下载只允许 Open → AppShared，fast 为 AppShared → Open。** 删除没有 shareId 的原 dt=3 owned 回退，防止任意公开文件 ID 读取服务端私有账号文件。原 29 + 20 个 frozen service/action 向量不改，安全差异单独断言，见 [公共下载安全差异](CLOUD189_PUBLIC_DOWNLOAD_SECURITY.md)。
2. **冻结旧版 folder-item 确有 missing-idMap 断链，当前工作区已修复。** 通过真实原始 Walk 解析目标且限定请求子树，不从名称投影伪造 ID、不擅自改成全分享查询；错误/超限不当空目录成功。parse_file 采用上游 metadata、拒绝目录、不追加 filename，并在 password-cookie 后重取元数据。旧 oracle 留作缺陷证据，修复差异独立测试，见 [action/index 边界](cloud189-actions-index.md)。
3. 公共状态不返回实际凭据；授权写操作/扫码创建与轮询需要后台登录和同源 POST。后台私有内容无 wildcard CORS，并带 no-store/no-referrer/frame DENY；**静态登录文档**仅将 referrer policy 改为 same-origin，以保留原生 POST 在普通 HTTP LAN 上的合法 Origin，仍禁止向外站发送 referrer。`Origin:null` 只在 Fetch Metadata 明确 same-origin 时通过，其它空/跨站/同站 metadata 不放行；这修复了本次真实浏览器发现的登录 403，不改原登录页 HTML。规范和回归见 [AUTH](AUTH.md)。
4. 本地 QR 图片不再将扫码会话发送到第三方；记录 JSON/通知等展示修复 XSS；后台搜索分页和移动 file_id action 有明确修正。布局/CSS 未重设计。
5. 短链只保存已验证的分享解析参数并执行本地目标，拒绝账户/管理参数，不合并入站 query/body；sid 保留和全部目录别名修复属明确差异。
6. 授权快照与 revision 原子读取，变更清空缓存；请求仍沿自己的旧账号快照完成，不得污染新账号的缓存。真实过期与失效重试仍需联测。
7. 安全转存组件默认关闭且不接公开解析，不能因 lib 已导出就称为完成。不能将独立官方 OAuth AccessToken/AppSecret 与现有 QR SessionKey/SessionSecret 按假设合并。
8. 其它网盘、完整分享/cursor/认证矩阵、天翼真实大分页、完整客户端下载/CDN 播放、大文件行为、同环境 CPU/RSS/长期延迟及线上回滚演练仍未完成。8788 正式迁移已验收；移动单样本 >200 分页和两宽目录并发基准也已通过，但不能泛化为所有场景已验收。管理员配置变更不自动撤销旧 session，旧 DB/sidecar 权限不强改，这是保护线上状态的保留边界。

## 下一步及所需授权

1. 8788 正式接管、独立 final audit 和精确临时文件清理已完成；Git 提交推送按独立步骤核验，结果以 Git 历史/回执为准。不重复执行已消费的迁移/清理 attempt，也不将原 cleanup 失败回执改写为成功。
2. 在明确的真实供应商操作授权范围内补单文件/目录文件、公开/保护分享、普通/fast 及 CDN 正文/播放等矩阵；天翼已有 S005/S006 取址 302，不能回退写成全部认证阻塞，也不能将其当作目录 ZIP 或完整文件下载成功。真实天翼 >200 同级分页和长期 p95/错误率、CPU/RSS 仍缺。
3. 真人扫码/真实长周期续期需要用户配合；真实创建/转存/删除另需专用测试根、可信同账户绑定、显式授权及恢复计划，绝不删除账号已有文件。受管账号解析可能按需续期，不能依赖旧环境开关宣称无供应商授权副作用。
4. 维护/调试工具、阶段脚本和回执的保留范围见 [TEST_DEPLOYMENT](TEST_DEPLOYMENT.md)。旧目标容器/镜像已清理，不能沿用历史留存描述或把保留脚本当成完整回退。保留私有 DB/备份/env/seed、工具链/缓存、源码与许可证；Git 按精确暂存、审计、提交推送分别核验，不做全局 prune 或旧 DB 回写。

## Git 与现网安全

- 本轮已获 8788 接管、指定清理与提交推送授权；部署、独立 final audit 及精确临时文件清理已核验。Git 结果以独立历史/回执为准，不从现网部署成功推导已提交或推送；旧测试容器留存记录不能作为当前库存。
- 当前待提交范围包含从先前阶段继承的大量 tracked/untracked 项目变更，不把它们误报为本轮文档改动。不要 `git add .` 或强制添加 ignored scratch；仅暂存核验过的源码、公共文档、合成测试、运行/维护脚本及许可文件。
- `.gitignore` 已覆盖 `.scratch/`、`.env*`、`data/`、`tests/private/`、默认 SQLite/sidecar 与离线 harness；这只是路径保护，不是内容脱敏证明。公共文件模式检查无高置信私钥/常见访问令牌命中，不等于对全部秘密的精确匹配或保证无泄露。
- `tools/audit-public.py` 仅检查 **staged** 文件，并读取 `.scratch/baseline/credentials.json` 中长度至少 8 的字符串作原样子串比较；不能覆盖未暂存/未跟踪文件、未载入的其它秘密、短密码或编码变体。须精确暂存后再运行并复查差异；删除项等不适用路径单独处理，不能把空暂存区通过当作全工作区通过。具体执行及结果以对应审计回执为准。
- `tools/verify-live.py` 加载私有账号，须显式传入 `--allow-provider-side-effects` 并事先获得相应目标/供应商副作用授权；不能用已移除的 `JXPAN_AUTH_REFRESH=false` 宣称禁续期。`verify-pages.py` 默认 synthetic、无 provider 账号。公共文档只保留脱敏样本标签/指纹，不提交原账号、分享/下载 URL、原部署源码、私有回执或 DB。
