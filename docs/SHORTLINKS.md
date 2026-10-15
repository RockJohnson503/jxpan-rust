# 有界持久化短链组件

## 接口与接线约定

```rust
ShortLinks::new(storage: Storage).await -> Result<ShortLinks>
links.create(original_url: &str).await -> Result<String> // 六位 shortcode，不是 URL
links.resolve(code: &str).await -> Result<Option<String>> // 仅 /?… 本地解析目标
```

`Result` 的错误类型为 `ShortLinkError`；字段/消息不含输入 URL、密码、SQL 或数据库底层报错。`None` 只代表不存在；数据库错误和不安全旧记录是 `Err`，不伪装为“不存在/过期”。

**`resolve` 返回值是经过校验的根路径 + query，绝不是原始 URL、外部跳转地址或任意 action。** `shortlinks` 已由 `src/lib.rs` 导出，共享 AppState 使用同一个 Storage 初始化组件。当前 HTTP 直接用 resolve 的安全 query 创建新的 Params，调用正常天翼/移动解析入口；不合并 `/s/{code}` 入站 query、表单或 JSON body，避免 `action/auth/token` 污染，也不把原始外层 URL 当作 Location。

`create` 接受完整 HTTP(S) 服务解析 URL；组件不硬编码部署 origin，**当前 `/api/create-short` 已在调用前验证其外层 origin 属于本服务**。生产/代理部署应配置 `JXPAN_PUBLIC_ORIGIN`（仅 HTTP(S) origin）；未配置时只按 Host 匹配 HTTP origin，不猜测转发头。读取旧记录时仍丢弃外层 origin，只能返回本地根路径。

## 当前 HTTP 入口

- `POST /api/create-short`：接受 JSON `{"url":"本站完整解析 URL"}`，先做同源检查与外层 origin 校验，再创建短链；不要求后台登录。成功返回 success/shortcode/msg，shortcode 为六位 code；请求/URL 无效为 HTTP 400，跨源为 403，正文超 16 KiB 为 413，存储/随机源/容量错误为 503。
- `GET/POST/HEAD /s/{code}` 及 `/s/{code}/{*rest}`：取首个 code 段，只使用持久化的安全参数重新解析；HEAD 的空 body 由 Axum 处理。入站 query/body 均不继承；合法的 type=down 仍可由正常解析结果产生下载跳转，不是向任意原始服务 URL 重定向。
- 不存在或非法 code 返回 404；危险旧数据返回 400，存储错误返回 503，不伪装成“不存在”。`/shorten` 未注册为创建端点。

## 冻结依据与兼容范围

权威部署 SHA-256：`47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118`。

本地冻结 oracle `tools/shortlinks-vectors.mjs` 与 `tests/fixtures/shortlinks.json` 保留以下原行为证据：

- `short_links` 表和过期索引；`generateShortCode` / `createShortLink` 六位 `A-Z a-z 0-9`，最多十次候选冲突。
- `getShortLink` 查询后增加计数，更新秒级 last_accessed。
- 完整 fetch 的实际创建端点为 **POST `/api/create-short`，不是 `/shorten`**；冻结版 `/shorten` 无专门分支。
- 原 `/s/…` 提取首个路径段，以当前请求 origin 的 `/` 转入主解析流程，而非外部重定向；允许 `/s/code/extra` 并保留入站 query。当前 router 保留路径提取行为，但明确丢弃入站参数。

表结构和默认值保留原样：`shortcode TEXT PRIMARY KEY`、`original_url TEXT NOT NULL`、`created_at INTEGER DEFAULT strftime('%s','now')`、`access_count/last_accessed/expires_at INTEGER DEFAULT 0`；索引名 `idx_shortlinks_expires` 保留。初始化验证关键列和主键，不删除或重写旧行。新记录保存标准化后的完整外层服务 URL，仅含已验证的解析参数：首值选择、d→id、fid 链去重压缩；被忽略的重复 URL 不保存，避免隐藏凭据。同一规范化 original_url 若已有合法 code，当前复用该 code；不同 original_url 不强制合并。

**过期列不执行过期检查。** 原 getShortLink 不读 expires_at 条件，原过期清理只删除 kv_store，因此 `NULL/0/-1/过去/恰好现在/未来` 的短链都仍存在并计数。404 文案“短链不存在或已过期”并不能证明执行过期逻辑。组件不新增 TTL，不因日志 retention 清理短链。

## 明确的安全收紧

1. 外层只接受 HTTP(S)、无 userinfo、解析后的 path 为 `/`、无 fragment；拒绝控制/空白字符和反斜杠。不猜补协议。
2. 必须有非空 `url`，其值必须是无 userinfo 的 HTTP(S) 支持分享入口，不能是任意 host、provider 登录/API 地址或自定义端口：
   - 天翼：`cloud.189.cn` / `h5.cloud.189.cn` / `m.cloud.189.cn`；`/t/…`、`/s/…` 或 `/web/share`、`/web/share.html`、`/share.html`、`/share`，且已实现的冻结分享识别器必须能提取 share code。
   - 移动：`yun.139.com` / `caiyun.139.com`；`/shareweb[/]` 或 `/link[/]`，且分享识别器必须能提取 link ID。允许 `#/w/i/字母数字ID` 分享导航片段及其密码 query。
   - 分享 URL 的 query / fragment query 仅允许 `code/shareCode/linkID/pwd/password`（不区分键大小写）；它们是分享标识/提取密码，不是账号登录密码。
3. 外层 query allowlist：`url,pwd,type,id,d,sid,fid,fid1…fid20`（最多 21 个目录参数名）。`type` 只允许空/`json`/`down`/`text`；这是 type 值，不接受独立 `json=1/down=1/text=1` flags。未知键直接拒绝，包括 `auth,c189_token,token,cookie,action`、账号凭据及任意管理/转存 action。
4. 重复键按 URLSearchParams.get 取第一项，`id || d`；`pwd` 保留（包含 Unicode、`+`、`&`），不记录到日志。目录链按 `fid,fid1…fid20` 取首值、去空、去重；去重后 **≤20 层**保持旧 canonical `fid,fid2…`，**完整 21 层**使用 `fid,fid1…fid20`，不得生成 `fid21` 或丢失一层。`tests/shortlinks.rs` 已覆盖 0–20 个数字别名加根目录的创建/解析往返及再次标准化幂等性；冻结的 96 个历史目标不改写。
5. **sid 保留是明确修正。** 冻结短链分支未从 original_url 提取 sid，因此新保存 sid 能完整恢复分享解析请求；不是静默声称完全等价。冻结实现继承入站 query、转发 auth/token/action 也被明确禁止。不安全旧行不会解析/计数，返回 InvalidUrl。
6. 随机源使用系统 getrandom，而非 Math.random；字符空间和长度兼容。64 字节拒绝采样，失败显式 Random；不无限重抽。
7. 只有唯一键冲突重试至十次；数据库/随机源/队列错误立即显式失败。旧 create 对任意数据库错误也重试十次，旧 get 将所有错误吞为 null；这是有意不同，避免隐藏故障和无意义重试。

## 边界、并发与持久化

- **总量上限 10,000 条**：同一规范目标先复用已有合法 code，无须随机生成，即使达到上限仍可复用。若需新目标且 COUNT 已达上限，返回 Capacity（HTTP 503）；不删除/淘汰旧短链，不借 retention/expiry 释放容量，不因旧库超量而自动清库。
- 完整输入、新存 URL、本地目标最大 **8192 字节**，最多 **64** 个 query 对，每个解码值最大 **4096 字节**。大尺寸旧行用 SQLite `CASE + length(CAST(original_url AS BLOB))` 在 rusqlite 复制前限制提取，按 UTF-8 字节而非 TEXT 字符计数。
- code 必须恰好六位 ASCII 字母数字。所有 SQL 参数化，路径、SQL 注入文本不作为 code 接受。
- 全部 SQL 走传入的 Storage::with_connection 有界 owner 线程；没有第二 actor、SQLite executor 阻塞、独立队列或后台清理线程。队列满返回 Storage(QueueFull)。
- 复用、COUNT 和插入在同一个 IMMEDIATE 事务内串行化，独立 Storage owner 也不能同时突破总量；`INSERT … ON CONFLICT(shortcode) DO NOTHING` 判断冲突，每次最多十个 INSERT，不覆盖旧映射。
- resolve 的读取、验证、计数更新处于 IMMEDIATE 事务；错误回滚，同一 SQLite 文件的多个 Storage owner 也不会丢失计数。只有安全命中的成功 resolve 计数。
- 数据存于独立 `short_links` 表，关闭重开保留映射/创建时间/计数。没有历史日志 retention 联动或自动删除。

总量上限只约束短链表，不等于全 DB 磁盘 quota；records 仍仅 retention，无总行数 quota。本轮未部署，旧 expires_at/session/权限均不静默迁移或强改。HTTP 另有 64 请求准入、body 15 秒 / handler 150 秒 / parse 120 秒；短链解析使用正常完整性/并发设置，见 [PARSE_SETTINGS](PARSE_SETTINGS.md)。真实大分页、天翼成功下载与 CDN 播放不因短链合成测试而通过。

## 完全离线验证

```sh
node tools/shortlinks-vectors.mjs --summary
cargo test --locked --offline --test shortlinks
JXPAN_LIVE_ORACLE=1 cargo test --locked --offline --test shortlinks
# HTTP 接线回归只向 loopback 发请求。
cargo test --locked --offline --test common_http
```

`JXPAN_LIVE_ORACLE=1` 沿用仓库命名，**这里只重跑本地冻结 Node oracle，没有网络或旧服务调用**，校验 fixture 与当前提取结果相同。common_http 则是独立 loopback HTTP 回归，不是云盘验证。

Oracle 用已有 acorn/astring 从冻结 readable.js 提取三函数和完整 fetch，在 Node VM 注入固定 Date/Math、MockDB、mock 主 handler、请求/响应对象；VM 没有 fetch/process/require/文件或凭据访问。全部 URL、密码和参数均是合成样本。fixture：`tests/fixtures/shortlinks.json`，包含 62 个字符映射、5 个创建/冲突/注入场景、6 个 expiry 场景、SELECT/UPDATE 错误、96 个分享 × type × GET/POST/HEAD 解析目标、实际创建端点、sid 丢失和入站参数继承证据。

Rust 离线测试覆盖 fixture 解析目标和密码、最后一次候选成功、十次碰撞上限、并发同 code 不覆盖、重开持久化、两个 Storage 并发累计 64 次访问、日志清理独立、真实过期语义、旧危险/超长行拒绝、参数/host/code 注入、数据库错误/更新回滚、随机失败和有界队列。`tests/common_http.rs` 已通过根 crate/router 覆盖创建 origin/同源检查、GET/POST/HEAD、额外路径段、入站参数丢弃和 HTTP 错误映射；使用禁用的移动 provider 与临时 SQLite，不请求真实云盘。接线及离线回归不代表真实分享下载或性能闭环。
