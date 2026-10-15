# 解析并发基准：当前移动样本与暂定默认 16

> 本文是 2026-10-14 基准及当日保存并发 4 的历史观察，不把当日数据改成新结果。2026-10-15 最新只读审计实际保存并发 **16**，本次切换前后全库保持，没有设置写入；最新发布/回归见 [CLOUD189_SHARE_REGRESSION](CLOUD189_SHARE_REGRESSION.md)。以下“本轮/当前保存 4/不能宣称有效 16”仅指原基准窗口，不覆盖新的明确观察。

## 结论与适用范围（2026-10-14）

本轮在目标机隔离临时容器中，对两个真实移动宽目录样本进行只读列表/完整 walk 测量。**68 次干净 benchmark walk 全部成功**（不计 inventory），各候选及修复前后完整 fingerprint 一致，活动 list future 峰值真正达到候选；受保护设置/session/auth、private seed 与已有服务保持，stats/history delta 为 0。

在 **1 / 2 / 4 / 6 / 8 / 12 / 16** 七个已测候选中，16 在两个样本的冷池中位耗时最低；新修复代码 shared 池 4 / 8 / 12 / 16 的复核也支持该方向。因此代码默认从 4 改为 **16**，范围仍为 **1–16**。这是**当前移动样本已测候选中的暂定性能选择**，不是所有整数、所有目录形态、所有供应商的最优值、供应商官方允许上限或长期稳定性证明。

**已保存合法设置优先**，缺键初始化使用默认且不写 KV。8789 最终只读审计确认已保存并发 **4**，按已验证构造逻辑推断当前有效值为 **4**（未直接观察授权设置 endpoint）；代码默认 **16** 未覆盖保存值。审计保持设置/session/auth/private seed 与已有服务，stats/history delta 为 0，没有 settings write、登录或 session 替换；证据 `.scratch/concurrency-fix-8789/saved-settings-audit.json` / `final-report.json`。不能用代码默认或 probe 的候选 16 宣称当前服务有效值为 16。设置契约见 [PARSE_SETTINGS](PARSE_SETTINGS.md)，发布与门禁见 [TEST_DEPLOYMENT](TEST_DEPLOYMENT.md) / [STATUS](STATUS.md)。

## 样本、版本与实验设计

仅使用内部脱敏标签，不披露分享 URL、密码、账号、文件/目录原始 ID、下载 URL 或凭据。

| 样本 | 目录（含根） | 文件 | 同一深度最大目录数 / 最大直接子目录数 | 作用 |
| --- | ---: | ---: | ---: | --- |
| mv02 | 33 | 172 | 31 / 31 | 宽度足以观察候选 16 |
| mv03 | 26 | 119 | 24 / 24 | 独立移动样本交叉复核 |

- 冷池批次使用修复前的生产代码；每样本 **22 次** benchmark walk：4 档 4 次，其余 1 / 2 / 6 / 8 / 12 / 16 各 3 次。两个样本共 **44 次**。
- 新代码 shared 池每样本 **12 次**：4 / 8 / 12 / 16 各 3 次，共 **24 次**。使用修复后的生产列表/完整 walk 路径；source 指纹为 `53e00a31c76ac839d973e6de649c0bfb11da5cbdc4171c0ca4091223579412f3`，与本轮发布 source 一致。probe 是独立诊断 binary，不把其 binary hash 当作服务 binary hash。
- 每批先做 inventory，检查宽度与完整目录结构；**inventory 不计入上述次数或中位数**。候选交错运行，不是把所有高并发都集中在最后测。
- 使用供应商列表/完整遍历，不走服务解析缓存，不执行普通 parse 计账。计时从 `provider_walk` 前至摘要/指纹完成，**包含该供应商路径的准备工作与根目录真实读取**；`root_preloaded:true` 仅表示 walker 复用本次已读根页、不重复请求，**不是根页耗时已被剔除**。客户端池建立、walk 间休眠不计入；该耗时仍不等同包含 HTTP 路由、普通解析缓存与 SQLite 收尾的端到端普通解析。
- probe 为 `tokio_current_thread` 异步 runtime，单次 walk deadline 120 秒，walk 间隔 2 秒；临时容器资源约束为 2 CPU、256 MiB、64 PID、非 root UID/GID 10001、bridge 网络。不是多核 CPU/RSS profiling。
- 没有额外应用重试；底层 HTTP 重试策略保持与 AppState 相同的 reqwest 默认行为，未改写成“禁用全部网络内部重试”。仅列表，不请求下载地址、不登录/续期、不做 CDN HEAD/Range/正文、owned 回退或云盘写入。

### “冷池”与“shared 池”的精确定义

- **fresh per walk（本报告简称冷池）**：每次 walk 使用新客户端连接池，不存在跨 walk 的该客户端连接池复用。这不保证 OS/DNS、供应商或其它上游 cache 都冷，也不是整机冷启动。
- **shared pool**：同批 walk 共用客户端池，允许跨 walk 连接复用；没有 socket 观测，**不能宣称实际观察到连接复用**。新旧代码、连接池模式、批次时段均有差异，不能把冷池与 shared 的绝对耗时差全部归因于修复或连接复用。
- `peak_active_list_calls` 测量的是**完整目录 list future**，不是精确活动 HTTP 请求数。list future 可包含分页及供应商路线处理，不能把“峰值达到 16”写成“16 条 HTTP 同时在途”。

## 冷池结果：修复前生产代码

单位：秒；为该候选 benchmark walk 的中位数，保留三位小数。

| 候选并发 | 每样本次数 | mv02 中位秒 | mv03 中位秒 |
| ---: | ---: | ---: | ---: |
| 1 | 3 | 21.774 | 16.209 |
| 2 | 3 | 11.784 | 9.353 |
| 4 | 4 | 6.330 | 5.218 |
| 6 | 3 | 5.079 | 4.071 |
| 8 | 3 | 3.950 | 3.404 |
| 12 | 3 | 3.131 | 2.859 |
| 16 | 3 | 2.544 | 2.653 |

两个样本在七个已测档位中均以 16 的中位数最低。尤其 mv03 的 12→16 收益已较小；少量重复测量不足以证明差异长期稳定或排除网络时变影响。未测 3 / 5 / 7 / 9 / 10 / 11 / 13 / 14 / 15，不能称为穷举全部允许整数。

证据：

- `.scratch/concurrency-bench-8789/mv02-cold-01.json`
- `.scratch/concurrency-bench-8789/mv03-cold-01.json`

## shared 池结果：修复后代码复核

每候选每样本均为 3 次，单位：秒，中位数。

| 候选并发 | mv02 中位秒 | mv03 中位秒 |
| ---: | ---: | ---: |
| 4 | 5.815 | 5.360 |
| 8 | 3.598 | 3.272 |
| 12 | 2.820 | 2.588 |
| 16 | 2.178 | 2.568 |

全部 24 次 benchmark walk 成功，完整 fingerprint 与冷池跨版本一致，各候选的活动 list future 峰值达到候选值。mv03 的 12 / 16 中位数接近，不将这组小样本结果写成显著性或长期 p95 结论。

证据：

- `.scratch/concurrency-fix-8789/mv02-shared-fixed.json`
- `.scratch/concurrency-fix-8789/mv03-shared-fixed.json`

## 完整性、保护状态与排除项

- fingerprint 算法为 `sha256-sorted-id-path-type-canonical-size-json-v1`，比较原始结构/ID 路径、类型与规范大小，不只比较文件数量。两个样本各自跨候选/版本 fingerprint 完全一致，既没有因加并发少列文件，也没有把部分树当成功。
- 44 次冷池 + 24 次新代码 shared = **68 次干净 benchmark walk**；各纳入批次均保持设置/session/auth/private seed、既有服务，stats/history delta 为 0。没有为了得到零增量重置/回扣统计或删历史。
- **排除旧 `mv02-shared-01` 批次**：虽然 24 次 walk 成功，但同一窗口保护/session、stats/history 有外部活动，不能把该窗口认定为干净的默认决策证据；不归因于 probe，不“修正”线上数据，也不把这 24 次加进 68 次。
- 成功 walk 不等于每次内部 HTTP 都零错误：证据没有精确 HTTP 请求计数，`http_zero_error_claim:false`。不能据此声称零 429/零 5xx/零内部降级，或给出无观测依据的 HTTP 错误率。

## 与真实分页及天翼下载验收分开

本轮另有新服务真实移动列表 **HTTP 200 / JSON code 200 / `tree_complete:true`**，7 目录（含根）、2260 唯一文件、最大直接文件目录 501，旧生产路径 `mobile_pagination` 失败而修复后成功。其 **15.124 秒**为单次端到端列表验收，不是本报告的并发基准，不与两个宽目录样本直接作倍数比较。移动当前分页契约是 caLength/coLength 严格核对本页实际计数、nodNum 不分页、满 200 或 active opaque cursor 继续，短页无 cursor 停止，预算/重复检测不放宽。

天翼真实生产列表 **HTTP/JSON 200、76 唯一文件**；没有可测宽目录，**没有天翼并发最优证据**。独立 diagnostic 密码校验成功且列表 76，但该响应没有分享 cookie；AppShared HTTP 400 `InvalidSessionKey`、OpenShared HTTP 400 拒绝且无 URL。sid + pwd 重验与 fresh client cookie 捕获/传递的代码回归不能写成真实下载成功；没有登录/刷新/session 替换。天翼成功下载与 CDN 门禁仍未通过。

对应证据：`.scratch/concurrency-fix-8789/real-listing-acceptance.json`、`.scratch/cloud-download-8789/diagnostic-v5-result.json`。普通列表验收的 total/success/history 增量与本报告 probe 的零计账分开，不能混用。

## 仍需验证

1. 天翼有效只读下载样本/授权及足够宽目录，真实大分页；完整移动 opaque cursor/分享矩阵。
2. 更长时间、更多分享和供应商的重复测量，尾延迟/p95、错误率、限流与稳定性；当前每候选 3–4 次不足以回答这些问题。
3. CPU/RSS、完整浏览器/CDN、真实扫码/续期与线上迁移/回滚演练。此次测试发布只影响 8789，不授权替换 8788。
