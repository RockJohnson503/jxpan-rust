# 私有解析历史、累计统计与保留期

本组件仅管理本 Rust 项目的解析历史，不是 stdout/stderr、Docker 或旧部署日志。使用共享 `Storage::with_connection` 的唯一 SQLite Actor；组件不另开连接、线程、定时器或连接池。`records` 已由 `src/lib.rs` 导出，并由共享 `http::AppState` 初始化及接入 router；主服务统一调度保留期维护。独立表为 `parse_records`、`records_settings`、`records_stats`。历史删除/保留清理 **只删除 Rust 的 `parse_records`**，不触碰 KV、账号凭据、QR、短链、缓存、旧 `parse_record_*` 或累计统计。

## 接入接口（均为 async）

返回类型中的 `Result` 是 `storage::Result`：

```rust,ignore
Records::new(storage: Storage) -> Result<Records>
records.insert(url: &str, pwd: &str, result: &serde_json::Value, cached: bool) -> Result<i64>
records.list(page: u32, limit: u32, kind: &str, search: &str) -> Result<serde_json::Value>
records.stats() -> Result<serde_json::Value>
records.record_cache_hit() -> Result<()>
records.record_request_outcome(success: bool, cached: bool) -> Result<()>
records.retention_days() -> Result<u16>
records.set_retention_days(days: u16) -> Result<()>
records.cleanup_at(now_ms: i64) -> Result<usize>
records.delete(id: i64) -> Result<bool>
records.clear() -> Result<usize>
records.delete_batch(limit: u32) -> Result<usize>
records.count() -> Result<u64>
```

- `new` 建表、初始化单行设置/累计统计并执行启动清理，不自行启动定时任务；已有配置及累计值不覆盖。可克隆，每次从 DB 读取设置，不缓存过时保留期。
- `insert` 返回单调 SQLite ID；同毫秒并发不会覆盖，同时为这一请求计账。HTTP 普通解析仅为 `Loaded` 插入历史；已有缓存命中及并发加入者只更新累计计数，不重复插入历史。调用方不得重复记账；插入失败需处理，不得伪造成功。
- `delete` 返回是否实际删除；不存在 ID 返回 false。`clear` 返回删除条数。`delete_batch` 每次按 `created_ms, id` 从最老记录删除 1..1000 条，非法值返回 `InvalidJson`；可重复到返回 0。这些 SQL 单语句删除具有 SQLite 原子性，失败会报错。
- `count` 是物理历史行数，可能包含等待定时清理的过期行；后台清空进度可用它。**前端列表 total 则始终按当前窗口、search/kind 筛选**。两者不是 lifetime total。
- `list/stats` 只返回数据，不包装 HTTP `code/msg/data`。当前 router 已负责授权及包络，解析入口已负责历史/统计写入；插入或统计保存失败返回 HTTP 503，不吞错继续当作成功。
- 保留离线时钟入口：`new_at(storage, now_ms)`、`insert_at(url, pwd, result, cached, now_ms)`、`set_retention_days_at(days, now_ms)`、`list_at(page, limit, kind, search, now_ms)`、`stats_at(now_ms)`。UTC Unix 毫秒允许 `0..=253402300799999`，否则 `Clock`。`stats_at` 的时间参数只校验，不改变累计统计。

## 当前 HTTP 接入

`/admin` 和 `/admin/` 共用以下 action；私有查询要求有效后台会话，修改还要求通过同源检查的 POST（URL 编码或 multipart 文本表单）。后台响应使用 `no-store`、`no-referrer`、`X-Frame-Options: DENY`，不开放跨域读取。

| action | 用途 / 参数 |
| --- | --- |
| `get_stats` | lifetime 计数；`/?action=get_stats` 另有公开聚合入口，不含私有历史 |
| `get_records` / `search_records` | `page`、`limit`、`type`、`search`；search_records 要求非空 search |
| `get_success_records` / `get_failed_records` | 固定类型分页查询，兼容前端 records 分页包络 |
| `get_log_settings` / `set_log_settings` | 读取 / 保存 `log_retention_days`，只接受整数 1..3650 |
| `delete_record` / `clear_all_records` | 正整数 `record_id` / 清空 Rust 历史 |
| `delete_batch_records` / `count_records` | HTTP 每批 1..10 条 / 返回剩余物理行数；组件 API 每批上限仍为 1000 |
| `cancel_clear` | 前端停止继续发批次请求的确认，不撤销已经提交的删除 |

后台另已接 `get_parse_settings` / `set_parse_settings`（concurrency 整数 1–16，默认 4）；这是独立 KV 设置，不改 records_settings/retention、历史或累计值。SQLite 已接受的设置命令在 HTTP 取消后仍执行并发布，失败不发布，见 [PARSE_SETTINGS](PARSE_SETTINGS.md)。

## 累计统计：不受日志清理影响

```json
{"total": 12, "success": 9, "failed": 3, "cached": 4}
```

`records_stats` 是独立单行表。`total/success/failed/cached` 从组件开始记账起为 **lifetime**，保留清理、手动删除、批量删除、clear、设置改变、重启均不减少。`total = success + failed`，`cached <= total`，计数列要求非负 INTEGER；溢出/损坏不能假成功。

插入记录、过期清理和四项计数增长在同一 `BEGIN IMMEDIATE` 事务内；任一步失败全部回滚。stats 直接读单行，不解密/聚合历史。

**保留冻结原版的两套不同分类，而不是擅自统一：**

- 请求累计统计：非缓存按原 `result.success` 的 JS truthiness 分 success/failed；缓存命中按原缓存分支增加 total、success、cached。标准输入为 success 布尔值。
- 历史列表/类型统计：按原 `msg === "解析成功" || code === 200 || code === "success"`，不优先采用 `result.success`。例如 `success=false, code=200` 的累计记为 failed，但历史仍列为 success；`success=true` 没有 code/msg 时恰好相反。冻结函数 oracle 和原请求计数代码片段均验证了此行为。

HTTP 对无 action、非指定文件 fast 下载的普通解析，按 `BoundedTtlCache::get_or_init_with_source` 的来源分别计账：

| CacheSource | 历史行 | lifetime 计数 |
| --- | --- | --- |
| `Loaded` | loader 调用 `insert(..., cached=false)` 保存一行 | insert 同事务增加 total，并按解析 result.success 记 success/failed |
| `Hit` | 不增加 | `record_cache_hit` 增加 total、success、cached |
| `Joined` | 不增加 | 按实际 Reply.request_success 增加 total、success/failed；cached 不增加 |

`Joined` 仅表示共享在途结果，**不等于成功缓存命中**；合并失败也必须记 failed。失败结果不保留为 ready cache，下次仍会重试。专用解析 action 和指定文件 fast 下载不走这套普通解析历史/统计分支。历史行数因此不等于累计请求数；`record_request_outcome(false, true)` 被拒绝。

旧未集成组件没有 lifetime 表，首次升级只在清理前用尚存的物理历史行估算初始化，之后不再重算；这**无法恢复之前已删除的累计值，也无法恢复旧组件抹掉的 URL/密码/data**。旧组件缺少 truncated 标记的历史在读取时明确补为 true，不能声称其仍可重解析。不读取/迁移旧部署 `jx_total` 或 `parse_record_*`；实际旧部署累计值迁移尚未实现，不宣称完整历史迁移等价。

## 默认 7 天，可配置持久化

- `records_settings.log_retention_days` 默认 **7**，整数 **1..3650**；0 不代表永久保留，非法值返回 `InvalidTtl`。配置不占用或改写 KV 键。
- `created_ms < now_ms - days * 86400000` 才删除，**恰在毫秒边界的记录保留**。天固定为 24 小时。
- 启动、插入、修改设置均执行基于时间索引的清理。设置更新与清理同事务，失败一起回滚；不同 handle 立即读到已提交设置。`src/main.rs` 还每小时调用共享 `AppState::maintain()`，无请求时也能清理；错过的 tick 不堆积，退出时停止维护。嵌入式调用方只创建 router 时须自行调度 maintain。
- 缩短立即清理，扩大不能恢复已删除行。扩大前若有尚未物理清理的过期行，可能重新进入窗口；小时维护之间 `list` 也会隐藏过期行，`stats` 不受窗口影响。
- 清理只做 DELETE，不做 VACUUM、安全擦盘或旧部署轮转；SQLite 文件不保证立即缩小。

## 历史查看、重解析与搜索

历史响应保留原前端的 `url/pwd/code/msg/data/timestamp/success/id`，额外增加 `cached` 和明确的 `truncated`。不再将分享 URL 改为 origin、抹掉分享密码或无条件令 data=null。

```json
{
  "success": [{
    "id": 42,
    "url": "https://example.invalid/share?pwd=synthetic-share#fragment",
    "pwd": "synthetic-share",
    "code": 200,
    "msg": "解析成功",
    "data": {"files": [{"name": "synthetic-file"}]},
    "timestamp": "2023-11-14T22:13:20.000Z",
    "success": true,
    "cached": false,
    "truncated": false
  }],
  "failed": [],
  "total": 7,
  "successTotal": 5,
  "failedTotal": 2,
  "page": 1,
  "limit": 10,
  "pages": 1
}
```

- 原 URL（包括分享路径、query、fragment）、分享密码、正常字符串 msg、标量 code 和 data 被保存；code 缺失/null 时按原前端默认 200，msg 缺失/空时按原默认“解析成功”（即使 success=false，保留原行为）；data 的 null/false/0/空字符串按原 truthy 规则显示 null。未知顶层解析字段并不是原前端历史字段，不加入响应。
- `success/failed` 拆分同一页，共不超过 limit；按 `created_ms DESC, id DESC` 排序。SQLite 自增 ID 取代旧毫秒 ID，准确 timestamp 保持 UTC ISO 毫秒字符串。
- `total` 是当前窗口/search/kind 的全匹配数；`successTotal/failedTotal` 是当前窗口/search 的分类型全量数，不加 kind。SQL 在 LIMIT/OFFSET **前**筛选、精确计数，只解密当前页。
- **有意修正原版分页 bug**：冻结 `d1GetParseRecords` 先 fetch 少量密文，再筛选，total 常是该批长度、类型 totals 可能来自陈旧缓存。75 条合成历史中，原第一页搜索只出现于旧记录的 5 个 needle 链接返回 total=0；新实现能准确返回 total=5 并正确分页。不声称这里逐字等价。
- kind 为 `""/"all"/"success"/"failed"`；其它 `InvalidJson`。page=0 -> 1；limit 限 1..50（0 -> 1），pages 至少 1，超末页返回空数组及准确 totals。原字符串 `page/limit="all"` 导出模式不属于此有界接口，未实现导出路由。
- search 为 HTTP 参数层已解码文本，不重复 URI 解码；trim 后 Unicode lowercase，最大 512 UTF-8 字节。私有 `search_text` 包含原 URL、分享密码、序列化的保留 data、msg、code，使用绑定 SQL `instr`，不把 `%/_/引号` 视为 SQL 操作符。它是私有明文索引，不是公开/脱敏遥测；组合字段文本并非任意复杂输入下与原 JS 单字段匹配逐字等价。子串仍需扫描窗口，不宣称 B-tree 点查。

## 私有存储与有界记录

- 共享 Storage 仅在**首次新建** SQLite 主文件时预创建为 0600；已存在数据库及 sidecar 不强改权限，不应称所有打开的旧库都是 0600。授权后台需要完整 URL/分享密码/data 来查看和重解析，禁止将此响应当公共日志提供。调用方仍需私有目录、HTTP 权限、备份/WAL 文件访问控制；后台 session 未绑定当前管理员配置版本，本轮保护活动 session，不等同完整权限审查完成。
- payload 复用原 `encrypt_for_kv` AES；其固定公开 key **仅为旧兼容混淆，不是真正的密钥保护**，私有数据库权限才是核心边界。search_text 明文，因此尤其不能发布数据库、dump 或索引。
- 递归排除明确账号认证键：先去 `_` / `-` 并转小写，因此 camelCase/snake_case/连字符别名均匹配。包括 `credentials/credential/authorization/cookie/cookies/set-cookie/access_token/open_access_token/app_access_token/refresh_token/session_token/session_key/session_secret/account_password/login_password/private_key/client_secret/cookie_login_user/SSON/share_session_cookies/loginInfo/mcloud_skey`。通用分享 `pwd/password`、下载 URL 和签名链接不抹除。不是语义秘密扫描；不应将任意账号对象或账户凭据塞进解析 msg/URL。
- `MAX_RECORD_BYTES = 1 MiB` 为明文 payload JSON 上限；账户键移除后对 data 有界遍历、复制，最大递归深度 64。保守的容器/键预算也可能提前触发截断，不宣称所有刚好小于 1 MiB 的复杂树都能存满。
- 大 data/过深树：保留 URL、分享密码、code/msg 标量，data=null，**truncated=true**；不能伪称完整历史，但仍可按原链接重解析。普通记录 truncated=false。强制字段本身超限、非标量 code 等无法合法有界保留的输入返回 `InvalidJson`，不插入/计数；msg 兼容范围为正常字符串。
- `MAX_PAGE_BYTES = 4 MiB`；逐条解密累加预算（预留 1024 字节响应包络），最终响应再次检验。即使页条数 <=50，过大页也返回 `InvalidJson`，**不静默裁掉记录、缩 limit 或伪造成功空页**；调用方应提示减小 limit。字节预算不是进程 RSS 上限。
- 读取密文 payload 前使用 SQLite `CASE + length(CAST(payload AS BLOB))`，上限 `(MAX_RECORD_BYTES / 16 + 1) * 32`（2 MiB + 32 字节）；在 rusqlite 复制大字段成 Rust String 前拒绝超大旧行，按 UTF-8 字节而非 TEXT 字符计数，不能只在解密后限制。
- **只有时间 retention，没有全表总行数/总字节 quota**。单条 1 MiB、单页 4 MiB 与保留 7 天都不保证 DB 总量有固定上限；高请求量或延长到 3650 天仍可能增长，不能称为全存储容量封顶。短链的 10k 上限是独立策略，不连带清理历史。
- DB/JSON/密文/队列错误返回静态 StorageError，不吞错假零统计/假空页，不回显 SQL、路径或上游秘密。

## 离线验证与仍未覆盖的范围

```sh
cargo test --locked --offline --test records -j 2
JXPAN_LIVE_ORACLE=1 cargo test --locked --offline --test records -j 2
node tools/records-vectors.mjs --summary
cargo clippy --locked --offline --test records -j 2 -- -D warnings
# HTTP 接线回归仅使用 loopback 和临时 SQLite。
cargo test --locked --offline --test common_http -j 2
```

测试只用临时 SQLite 和 `.invalid` 合成链接，未使用真实账号或真实分享。覆盖 7 天毫秒边界、1/3650 天/非法值、配置持久化与即时缩短、启动/插入清理、扩大不可恢复、事务回滚、删除范围/幂等/批次、累计计数不随删除下降、重启/旧组件估算升级、64 路同毫秒并发、精确 SQL 搜索分页、只解密所选页、0600、账号键排除/分享字段保留、2 MiB 截断及分页字节超限。

`tools/records-vectors.mjs` 从冻结源码提取真实 `getStatsFromKV/saveParseRecord/d1GetParseRecords` 和两个请求计数片段，在隔离 VM/内存 DB stub 运行；原 worker SHA-256 固定为 `47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118`。`tests/fixtures/records.json` 固定函数/片段 SHA、10 组完整前端字段向量、4 次原累计计账事件、原默认 604800 秒和分页 bug 样本；`JXPAN_LIVE_ORACLE=1` 只重跑本地冻结 oracle 并精确比较，不连接旧服务或云盘。`tests/common_http.rs` 覆盖权限、设置、历史查询/删除、重启持久化和累计统计；`tests/cache.rs` 区分 Hit/Joined/Loaded，`src/http/mod.rs` 的无网络单元回归验证两个合并失败请求记两次 failed、cached=0 且只保留一行历史。

**已完成接线，仍非全部业务验收：** lib 导出、HTTP 登录/权限与后台 action、普通解析历史/缓存来源记账和小时清理已接入；历史测试实例曾验证 retention 持久化，记录见 [STATUS](STATUS.md)，不应继续声称整个后台未上线。**本轮新并发/完整性/SQL 字节限制尚未部署**，全套重测仍由主流程完成。旧部署累计值/历史迁移、真实重解析/删除全矩阵、总磁盘 quota 与同环境性能未完成；不能将离线/loopback 通过替代真实供应商下载、真实大分页或 CDN 播放验收。
