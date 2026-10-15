# 解析并发、完整性与资源边界

本文描述当前解析并发、完整性与资源契约。当前仅部署 8789 的 `jxpan-rust:test-3d1cee097916`（2026-10-15 天翼修复），本契约与上一部署保持；不代表真实供应商全场景验收。最新发布/设置观察见 [STATUS](STATUS.md) / [CLOUD189_SHARE_REGRESSION](CLOUD189_SHARE_REGRESSION.md)，默认选择依据见 [CONCURRENCY_BENCHMARK](CONCURRENCY_BENCHMARK.md)。冻结旧版的 4/6 并发、400 节点与第一页列表仅为历史差分依据。

## 后台设置与持久化

`/admin`、`/admin/` 的 `get_parse_settings` / `set_parse_settings` 均要求有效后台会话；修改还要求同源 POST。表单参数为 `concurrency`，只接受 **1–16 的整数**，代码默认 **16**；非法值为 HTTP 400，存储失败为受控 503。已保存的合法值优先，返回的 `concurrency` 是有效值，不必等于 `default_concurrency`。缺键时读取/保存成功的后台 `code/msg/data` 包络中 `data` 示例：

```json
{"concurrency":16,"default_concurrency":16,"min_concurrency":1,"max_concurrency":16}
```

- 设置键是共享 SQLite `kv_store` 的 **`rust_parse_settings`**，值为 `{"concurrency":N}`，使用现有 KV 兼容编码；**无 TTL**（`expires_at=0`）。
- 缺设置时启动使用 16，**不为默认值写 KV**；已有合法设置优先，不因默认变化而改写。已保存的非法值使初始化明确失败，不静默改写。
- 保存先写 SQLite，成功后才发布进程内共享预算；写失败不发布。更新由共享串行锁排序，锁与发布动作归已接受的 SQLite 命令所有。
- HTTP 等待者在命令已入队后取消，不能撤销该写入，也不能留下“DB 已提交但运行时未发布”的状态；后续更新须等前次发布后才能继续。取消发生在命令接受之前则不承诺执行。
- 只调整调度，不清缓存、历史、授权、短链或管理员 session，不重新写其它业务设置。该串行/运行时发布保证限于同一共享 AppState；不宣称多个独立服务进程实时同步预算。

**1–16 是应用侧防御/调度范围；默认 16 是两个移动样本在七个已测候选中的暂定性能选择，不是全供应商最优或官方上限。** 68 次干净基准见 [CONCURRENCY_BENCHMARK](CONCURRENCY_BENCHMARK.md)。2026-10-14 只读审计保存 4 的回执原样保留；2026-10-15 最新只读审计实际保存 **16**，且全库与该次切换前相同，没有设置/session/auth 写入、stats/history 增量为 0，见 `.scratch/cloud189-fix-8789/final-audit.json`。有效 16 依据已验证构造逻辑推断，非授权 endpoint 直接观测；不能仅凭代码默认/探针候选判断在线值。本次天翼探针独立固定 4 控制负载，不写回服务配置。

依据：`src/settings.rs`、`src/http/mod.rs`、`src/http/state.rs`。

## 两层并发，不是一个统一 RSS 上限

### 共享供应商 I/O 预算

天翼与移动的**公开分享客户端**共享同一个 `UpstreamLimit`。每次调用在 `send` 前准入，直到完整响应 body 被读取、报错或取消才释放，不在收到 headers 后提前释放。目录页、分享元数据、提取码与公开下载地址请求均经过各客户端的此入口；返回直链不跟随到 CDN。

运行时提高上限唤醒等待者；降低上限不取消已有 I/O，已有调用可能短时多于新值，待活动数下降到新上限以下才接纳新调用。permit 随 future 释放，无遗留后台请求。

**授权刷新和 QR 登录客户端未纳入此预算。** 因此 concurrency=N 不能解读为全服务所有出站网络请求都不超过 N；转存/自有目录能力也不是公共分享路径。

### 本次完整 walk 的调度快照

生产 `tree::walk_complete` 使用 `FuturesUnordered` 滚动调度：完成的目录立即处理并补入子目录，不等待同批最慢的兄弟目录；所有 futures 属于本次请求，失败/取消时释放。

service 为本次 walk 取固定 concurrency 快照（缺设置默认 16，最多 16；已保存值优先），本轮 walk 不随保存设置改动其调度窗口；底层每次 I/O 的共享预算仍实时遵循新设置。这一区别用于避免遍历期间调度参数漂移，不会让旧 walk 绕过全局 I/O 准入。

## 完整性：完成才成功，错误不是空目录

- 只有全部目录和每个目录的分页成功后，完整树路径才输出成功；当前移动 folder 响应带 `tree_complete:true`，天翼 `folder_response` 仅有 `tree` / `sid`，**没有统一的完整性 flag**，不能把移动字段泛化到所有供应商。上游失败、畸形 metadata、页重复、循环或资源超限均明确报错，**不返回部分树作为成功，也不缓存失败树**。
- ID 在全次 walk 中唯一：重复文件 ID、跨父目录重复 ID、文件/目录共用 ID、根回环都拒绝，不能依完成顺序挑“第一次匹配”。单目录页间同样拒绝重复/重叠 ID。
- 文件/目录标志采用明确布尔/数字/字符串规则，`"0"`、`"false"` 不按 JS truthiness 当目录。未知文件大小是 **`null`**，不是伪造 0；真实 0 有效。非负整数字节数超过 JS 安全整数时以字符串返回，合法十进制字符串保持字符串，非法大小明确失败。
- 名称投影使用 `nested_tree_complete`，重名先追加 ID，仍碰撞时继续加序号，保证不会覆盖既有文件/目录（包括本来就带后缀的名称）。目录索引来自原始 Walk，不能从显示名称树反推。

### 独立上限（任一先触发即失败）

| 层级 | 当前上限 / 语义 |
| --- | --- |
| HTTP 准入 | 64 个处理/响应，占用保持至响应发送或丢弃；满额受控 503，不无限排队 |
| 入站参数/body | 读取 15 秒；普通表单 64 KiB，短链创建 JSON 16 KiB |
| handler / parse | handler 150 秒；parse 的供应商工作 120 秒；超时受控失败，不是部分成功；finish / 持久化在供应商预算外执行一次 |
| 供应商单响应 | 2 MiB，逐 chunk 检查，不仅信任 Content-Length |
| 单目录分页 | 每页 200；最多 100 页、累计 20,000 项、4 MiB 保留估算 |
| 完整 walk | 最多 10,000 个目录（含根）、20,000 entries、8 MiB 保留估算、96 层（根为第 1 层） |

这些是不同层的限额。walk/列表的字节数含行与容器开销的保守估算，缓存另有限额；不是进程 RSS 硬封顶，也不保证任何拥有 10,000 目录的分享都能成功（其它上限可能先触发）。缓存、KV 与记录限额另见 [CACHE](CACHE.md)、[STORAGE](STORAGE.md)、[RECORDS](RECORDS.md)。

### parse 超时与计账收尾

`parse::run` 的 120 秒 timeout 只包围供应商工作，得到成功/失败/超时结果后，`finish` 与持久化在 timeout 外执行一次，不把排队等待 SQLite actor 的收尾误当供应商超时再写一遍。synthetic actor stall 修复前复现两条历史写入，修复后为一条；普通解析与 fast 解析的超时计账回归通过。该边界不取消 handler 150 秒限时，也不是无限收尾或所有客户端取消场景的承诺；不清零、不回扣旧统计。

## 分页契约与未验证项

- 天翼第一页按既定 API/mode 顺序回退；某路线成功后固定该路线分页，后页失败不能混入另一 API/mode 的页。总量变化、重复项、超量或无法完成都报错。
- 天翼成功空目录可省略 `folderList` / `fileList`，但必须明确总量 0，且已出现的非 null `fileListSize` 必须为 0。嵌套 count/total 保持权威并与顶层总量核对；缺失/非零/冲突总量、畸形数组或失败码均拒绝。该兼容来自同一真实 v3 样本与独立先红后绿回归，不能泛化成“缺列表就是空成功”。
- 移动使用有序 JSON 构造与 `bNum/eNum` 页窗口；`caLength`、`coLength` 是各列表**本页计数**，出现时严格核对该页实际数组长度，不按累计 total 解释；**不将 `nodNum` 用于分页**。active `nextPageCursor` 为 opaque JSON，下一页原样传递；重复 cursor/重叠页拒绝。任一 ca/co 列表满 200 或存在 active cursor 就继续；仅两个列表均短页且无 cursor 时停止，满 200 必须请求尾页，不能用相等的本页计数提前停止。预算、空页却继续/重复检测不放宽。
- 真实移动大分享旧生产路径 `mobile_pagination` 失败，新版 HTTP 200 / JSON code 200 / `tree_complete:true`，7 目录（含根）、2260 唯一文件、最大直接文件目录 501；**单样本 >200 分页已验收**，单次 15.124 秒不是性能基准。仍未覆盖所有 opaque cursor 放置/语义、分享场景或天翼大分页；`have_page:false` 本身不是完整性证明。

## 复现与审查边界

```sh
cargo test --locked --offline --lib settings -j 2
cargo test --locked --offline --test tree_complete --test cloud189_http --test mcloud_http -j 2
cargo test --locked --offline --test cloud189_service --test mcloud_service --test common_http -j 2
```

下段 279 项 / 35 项 / 76 文件等为 2026-10-14 历史验证，原样保留；2026-10-15 的 288 项 Rust、Clippy 双工具链结果、40 分享回归和当前保存 16 观察见 [CLOUD189_SHARE_REGRESSION](CLOUD189_SHARE_REGRESSION.md)。

以实际测试输出为准。离线 oracle/frozen fixture 保持旧预期，完整性修复单独声明差异，不把修改后的 JSON 归一化成“完全等价”。本轮主代理验证 **279 项 Rust 全套**、fmt、严格 all-target clippy、**3086 前端 synthetic checks**、隔离页面、**35 项 deployer + 14 项 orchestration** 测试通过，仅更新 8789。真实移动完整树 2260 文件、天翼列表 76 文件通过；天翼 diagnostic 密码校验成功，但 AppShared HTTP 400 `InvalidSessionKey`、OpenShared HTTP 400 且无 URL，**成功下载门禁仍未通过**。历史空目录、两文件/302、270 项与 test-1807efaa0aed 记录保留在 STATUS/TEST_DEPLOYMENT，不替代本轮结果；不宣称所有供应商完成或线上回滚演练通过。

公共天翼下载仅 Open/AppShared，不回退 owned、转存或删除；sid + 非空 pwd 不再跳过密码重验，fresh client 捕获分享 cookie 并用于 AppShared/OpenShared。真实 diagnostic 没有收到分享 cookie，代码 cookie 回归不等于真实下载成功。不做 Range 探测、不拉文件正文，直接供应商 URL 302。刷新 `Err` 不留 ready cache；移动原有一小时失败退避保留。管理员 session 尚未绑定当前配置版本，旧数据库权限不自动强改。迁移、真实扫码/续期、天翼成功下载/大分页、完整移动 cursor 矩阵、CDN 与 CPU/RSS/长期尾延迟仍需独立验收。
