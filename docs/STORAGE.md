# 本地 SQLite KV 存储（独立基础模块）

本模块提供本地 KV 与旧 JxPan KV AES 协议兼容基础。**不代表全套持久化完成**：未接入 HTTP、登录态、解析缓存、统计、记录分页或短链，也未迁移 Cloudflare D1 数据。测试只使用合成数据与临时本地数据库，无远程数据库访问。

## 集成要求与 API

父项目需启用 `rusqlite = { version = "0.37", features = ["bundled"] }`、Tokio `sync`（现有 `rt`、`macros` 足够），并导出 `storage` 模块。本任务不修改 Cargo/lib/HTTP。

```rust,ignore
let kv = Storage::open("local.sqlite", 64).await?;
kv.put("example", serde_json::json!({"offline": true}), Some(3600)).await?;
let value = kv.get("example").await?; // Result<Option<serde_json::Value>>
kv.delete("example").await?;
let changed = kv.compare_and_swap("example", None, Some(serde_json::json!(1)), None).await?;
kv.update("example", |current| {
    Ok(Update::Put {
        value: serde_json::json!(current.and_then(|v| v.as_u64()).unwrap_or(0) + 1),
        ttl_seconds: None,
    })
}).await?;
kv.close().await?;
```

- `put` 的 TTL 为 `Option<i64>` 秒；`None`、`Some(0)` 不过期。与旧源码一致，负数亦可用于已过期时间；时间相加溢出返回静态 `InvalidTtl`，不写入。
- `get` 返回 `None` 表示缺失/过期/旧空字符串行；JSON `null` 则是 `Some(Value::Null)`。
- CAS 在 `BEGIN IMMEDIATE` 事务中比较**解码后的 JSON 值**，不比较序列化字符串或密文字节。`expected: None` 也匹配过期行；`replacement: None` 删除。
- `update` 回调在同一事务、数据库工作线程上执行一次。`Keep` 保留值与原过期时间，`Delete` 删除，`Put` 替换值并重设 TTL。返回最终值。
- 回调返回错误或 panic 时回滚事务，panic 映射为 `UpdatePanicked`。未自定义全局 panic hook；应用自行保证 hook 不输出敏感信息。回调禁止网络、阻塞工作、异步等待和再次调用本存储。运行时检查工作线程重入并返回 `SameThread`，防止回调通过 `block_on` 自等死锁。

## 专用线程、有界队列与生命周期

每次 `Storage::open` 创建一个专用 OS 线程，该线程拥有唯一的 `rusqlite::Connection`。打开数据库、建表、schema 检查、SQL、JSON 编解码、AES、事务、关闭连接均在该线程执行，不在 Tokio executor 上执行 SQLite I/O。多个 clone 共享这个线程；两个独立 `open` 是两个实例，调用方应对每个数据库路径维持一个共享实例。

命令队列为 `std::sync::mpsc::sync_channel`，提交使用非阻塞 `try_send`，回应使用 Tokio oneshot。容量只计排队命令，另可有一个正在执行的命令；满队列立即返回 `QueueFull`，不启动额外数据库线程、不无限排队、不隐式重试。容量零被拒绝。SQLite busy timeout 固定 250 ms，锁冲突超过时限返回 `Database`；所有等待发生在工作线程。调用方如需重试，应实施自身有界策略。

`close().await` 对所有 clone 生效，拒绝新命令，先排空已接受命令，再在 `spawn_blocking` 中 join 工作线程。并发 close 安全。最后一个句柄 Drop 只断开队列，绝不在 executor 中 join；线程排空后自然退出。要求确定释放数据库时显式 await `close`。没有定时清理线程或后台守护进程。初始化失败、取消 open 也不留下永久等待收件的线程。

调用 future 在命令入队后被取消，不会撤销已接受的写操作。取消 close 不重新开放实例。原子更新回调必须快速结束，否则会阻塞本实例队列与关闭流程；这是调用方契约，不是用于运行任意用户代码的沙箱。

## Schema 与过期语义

实际旧 D1 schema 来源：`.scratch/baseline/readable.js:3343`：

```sql
CREATE TABLE IF NOT EXISTS kv_store (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    expires_at INTEGER DEFAULT 0,
    created_at INTEGER DEFAULT (strftime('%s', 'now'))
);
```

这里的 expiration 概念实际名为 **`expires_at`**，不是列名 `expiration`。启动检查所需列名/类型、value 非空约束及 key 主键；已有不兼容表返回 `Schema`，不静默迁移或重建。`created_at` 是原协议结构的一部分；写入使用原来的 `INSERT OR REPLACE`，因此替换会重建该时间。无 D1 的其他统计/短链表和索引。

过期判断完全保留 `.scratch/baseline/readable.js:3486`：

```text
expires_at > 0 && expires_at < floor(now_in_seconds)
```

因此到期秒**相等时仍有效**，下一秒才失效。0、负数、NULL expiration 不过期。读取到过期行会在工作线程删除；CAS/update 的读取也遵循此规则，且清理受事务回滚控制。没有后台全表清理。

## 数据库文件权限（Unix）

- 普通磁盘路径先在专用数据库线程通过 `std::fs::OpenOptions`、`create_new(true)` 与 Unix `OpenOptionsExt::mode(0o600)` 独占创建空文件，再交给 SQLite 初始化。权限在首次创建时生效，不存在先以默认宽权限创建、再 chmod 的窗口。进程已有 umask 可以进一步减少权限，本模块不读取或修改全进程 umask。
- 已存在文件（包括空文件）不 chmod、不截断、不改变已有权限。旧数据库若权限过宽，须由部署方另行审查和收紧，不因打开本模块自动迁移权限。Unix 上 SQLite 打开已预留的磁盘文件时关闭 `SQLITE_OPEN_CREATE`，若文件在两步之间消失则失败，不让 SQLite 用默认权限重建。初始化失败可能留下新建的空文件；不擅自删除。
- `:memory:`、SQLite 内存 URI（如 `file::memory:`、`file:name?mode=memory&cache=shared`）与空路径临时数据库保留 SQLite 行为，不预创建字面路径文件。Unix 磁盘数据库仅接受普通文件路径；磁盘 `file:` URI 返回静态 `Database` 错误，避免 URI 实际目标与预创建路径不一致而绕过保护。非 Unix 平台保留原有 SQLite 打开行为，不承诺 POSIX 0600。
- 本模块不启用 WAL，也不更改已有数据库的 journal mode。SQLite 默认 Unix VFS 新建 rollback journal、WAL 和共享内存（`-shm`）文件时通常复制主数据库的权限；此路径下 SQLite 可通过 `fchmod` 消除 umask 的影响，**不能依靠更严格 umask 自动收紧旧宽权限库的 sidecar**。因此 0600 新库有助于保护新建 sidecar，但本模块并不逐个创建或 chmod 这些文件。已有 sidecar、旧宽权限数据库、其他 VFS、ACL 和临时文件均不能据此宣称统一受 0600 保障。不要在连接存活时手工删除 journal/WAL；备份、复制和权限审计应涵盖必要的 sidecar。
- 数据库父目录应由受信任用户控制（建议 Unix 0700），避免其他用户替换路径或放置符号链接；这里不是对恶意可写目录的路径竞态防护。0600 也不防御同 UID、管理员、备份泄漏或未受控 ACL，不替代应用密钥管理或凭据安全设计。

## AES KV 兼容范围与安全边界

- 使用 RustCrypto `aes 0.8` 的 AES-128 block API；没有手写 AES 轮函数。
- 固定旧协议兼容常量 `LsPro-JxPan` 的 UTF-8 字节右补零到 16 字节。
- AES-128-ECB、PKCS#7 padding、UTF-8 JSON、无 IV、输出小写十六进制；读取亦接受大写 hex。
- `encrypt_json_text` 保持调用方 JSON 原文字节，用于与旧 `JSON.stringify` 精确密文差分。
- `encrypt_for_kv` 使用 `serde_json` 序列化。固定样本的字段顺序、中文/emoji、转义、数字及 padding 均验证；不声称对任意 JavaScript 值实现完整 `JSON.stringify` 替代。特别是浮点整数表示、极小/大数指数格式、超出 JS 安全整数、手工构造的整数键对象顺序可能产生不同 JSON 字节，尽管双方仍能解码。若需要字节级复现，使用旧序列化原文加 `encrypt_json_text`。
- 读取兼容以 `{`/`[` 开头的旧明文 JSON，以及合法旧加密 JSON。普通非加密字符串不是合法 JSON KV 密文，会返回明确错误；不将坏数据静默吞成成功。
- Rust typed KV 的 put 对**全部 JSON 类型**加密并保留读取类型，包括字符串、布尔、数字及 null。旧 `storePut` 对非对象走 raw，旧 `storeGet` 又将解密后数字/布尔/null 丢成 null；这些旧 wrapper 特例不复制进 typed KV，且本模块未替换这些 HTTP 行为。
- 畸形 hex、错误 PKCS#7 padding、非法 UTF-8/JSON 比旧源码严格：返回静态错误，而非模拟 JS `parseInt`/宽松 unpad/TextDecoder replacement。
- **ECB 与公开固定 key 不提供秘密保护或完整性认证**；这里只为旧协议兼容，不可宣称安全凭据保管。数据库文件与访问权限仍需应用管理。本代码未包含真实用户凭据或生产密钥。

存储代码没有 stdout 日志，没有带 key/value/路径/SQL/底层错误的 Debug 输出。错误枚举及 Display 均为静态明确文本。含数据的 Storage/Update/命令不实现 Debug。

## 可复现离线验证

```sh
node tools/storage-vectors.mjs --write
cargo test --lib storage::tests
cargo test --test storage
```

生成器验证冻结 `_worker.js` SHA-256：`47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118`。使用 Acorn 从冻结 readable.js 提取原始 `AES128ECB`、`encryptForKV`、`decryptFromKV`、`d1Get` 和协议常量到纯 VM。保留原始 obfuscator `__p_G0UC_dummyFunction`，避免 AES 路径中的 `in` 判断触发 ReferenceError。不执行整个 worker；VM 没有 fetch/process/require/文件系统，无读取凭据路径。

`fixtures/storage.json` 含 21 组纯合成 AES KV 向量、6 组冻结时钟过期真值，附部署与提取函数 hash。生成器 stdout 只输出 hash/数量，不输出 value 或密文。

Rust 测试覆盖：原始密文逐字一致、双向解码、大小写 hex、Unicode、嵌套 JSON、PKCS#7 边界、Unix 首次创建（SQLite 初始化前）0600、重开保持 0600、已有库/空文件保留权限、内存/临时库行为、磁盘 URI 防绕过、临时文件数据库重开、旧明文 JSON、坏密文、过期删除、严格等秒边界、TTL 溢出、CAS/null/过期比较、多任务原子计数、回滚、重入禁止、队列满受控失败、当前线程 Tokio executor 在工作线程暂停时仍运行、显式关闭排空、clone 全局关闭、schema 不兼容及打开失败。所有 SQLite 数据均是本测试自建的临时本地数据。
