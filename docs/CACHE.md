# TTL 缓存与相同请求合并

模块：`jxpan_rust::cache::{BoundedTtlCache, CacheConfig, CacheError}`。该模块独立于 HTTP 和 provider，可分别创建解析结果缓存与下载链接缓存，在 `AppState` 中共享。克隆缓存共享条目与请求合并状态。

## 路由接入约定

- 解析、下载和目录使用独立缓存实例；当前 `AppState` 的 TTL 均为 **600 秒**，下载 TTL 对齐原版 play 缓存的 10 分钟。解析字节预算为 8 MiB，下载/目录各 256 KiB；均限制 64 条目、8 个活动加载。十分钟是应用保留策略，**不代表已验证上游签名链接必定有效十分钟**；若之后按供应商有效期收紧策略，需要单独验证并明确行为变化。
- 键必须包含所有影响返回值的参数：provider、规范化后的分享标识、密码、文件 ID、完整目录链、具体下载方式、**有效凭据的作用域**。不要使用容易发生歧义的简单字符串拼接；可以使用 JSON 元组编码。不同凭据、密码或目录链不能共用条目。不要记录这些键。
- `get_or_init(key, load, cache_weight).await` 返回 `Result<Arc<V>, CacheError>`。闭包要拥有请求所需的参数和 client，不能借用临时 HTTP 请求。
- `cache_weight(&V) -> Option<usize>`：仅成功、完整且有效的上游结果返回 `Some(weight)`；失败、空链接、认证拒绝、非法下载 URL 等返回 `None`。并发等待者仍共享失败结果，后续请求重新加载。
- `KeyTooLarge` 应转换为受控的客户端参数错误；`InFlightLimit` 应转换为 429 或 503。不要在容量耗尽时无界绕过缓存调用上游。
- 下载 URL 应在进入缓存前验证协议、主机和 header 编码。
- 后台解析并发设置与缓存加载限额是两个独立层：`concurrency` 默认 4、范围 1–16，只调共享供应商 I/O 预算，不改变三个缓存各 8 活动加载/64 条目的配置；保存不清缓存、账号、历史或短链。每个完整 walk 固定本次调度快照，底层 I/O 实时受共享预算约束，见 [PARSE_SETTINGS](PARSE_SETTINGS.md)。
- 生产完整树必须等 `walk_complete` 与所有分页成功；失败/循环/全局重复 ID/限额错误不能以部分树进入 ready cache。两网盘单响应/单目录/Walk 上限独立于保留缓存预算，不要用 8 MiB 缓存上限宣称所有进行中分配有界于 8 MiB。
- 授权刷新合并缓存只给 `Ok` 结果可保留权重；`Err` 只共享给当前等待者，下一请求可重试，不把错误驻留 ready cache。移动持久化的一小时失败退避仍是 provider 策略，可能表现为正常 `Ok(false)`；不为立即重试移除退避。auth 刷新/QR 网络不在公开分享的并发预算内。

示意（`V = Result<Option<String>, UpstreamError>`，错误类型不需要 `Clone`；示例省略 URL 校验）：

```rust,ignore
let result = downloads
    .get_or_init(
        scoped_key,
        move || async move { client.download_url(&sid, &file_id).await },
        |result| result.as_ref().ok().and_then(Option::as_ref).map(|url| url.capacity()),
    )
    .await?;
```

## 边界与失效语义

- TTL 从加载完成开始计算，读取不延长 TTL；`expires_at <= now` 时失效。
- 同时限制可保留的条目数、值权重加键长度的总预算、单键长度、不同活动加载数。LRU 淘汰最近最少使用的值；超大结果可返回给当前等待者，但不留存，不驱逐已有正常值。
- `max_entries = 0` 或 `ttl = 0` 禁用结果保留，但仍合并相同请求；`max_in_flight = 0` 拒绝所有新加载。
- 一个等待者取消不影响其它等待者；全部取消时底层加载立即随 future 释放，不启动后台任务。弱引用登记会在下次操作时清理。不同请求可以并行，锁不跨越 await。
- `invalidate(key)` / `clear()` 删除可用结果并使旧加载与该键脱离。旧等待者仍拿到自己的结果，但旧结果不能覆盖新的结果，不能删除新的活动加载，也不能重新填充缓存。
- 被失效操作脱离的加载仍占活动加载配额，直到完成或全部等待者取消。因此反复失效不能绕过并发上限。
- 无后台清理任务。过期条目在下一次 `get`、`get_or_init`、`invalidate` 或 `stats` 时回收；闲置期间保留的数据始终受预算和条目数约束。

## 内存预算说明

`max_bytes` 是应用提供的**保留值权重 + 键长度**预算，不是整个进程的 RSS 上限。值以 `Arc` 返回，不会为每个等待者复制完整内容；但调用者持有的旧 `Arc`、进行中的上游响应、HashMap/共享 future 元数据及等待者数量不计入该值预算。条目数、键长度与活动加载数另行限制元数据数量。

权重应覆盖值拥有的缓冲区容量，而不是只使用序列化后的长度估计复杂 `serde_json::Value` 的堆占用。希望准确控制大型解析结果的保留缓冲区时，可缓存序列化后的 `Vec<u8>` / `Box<[u8]>`，按容量 / 长度计权。HTTP 层还必须独立限制上游正文、请求并发与遍历规模；本缓存不会自动约束 loader 内部的分配。

## 本轮边界与未完成

HTTP 另有 64 准入、body 15 秒 / handler 150 秒 / parse 120 秒；公开分享 response 2 MiB、目录分页累计 4 MiB 估算 / 20k items、完整 Walk 8 MiB 估算 / 10k 目录 / 20k entries / 96 层，任一限额先触发即失败。这些限制不替代 RSS/延迟实测。公共天翼已删除 Range 一跳探测，302 直接供应商 URL，不把 CDN 内容缓存在服务内。

本轮未部署，不声称真实大分页、天翼成功下载或 CDN 播放已通过；历史 600 秒 TTL 实机更新仍是历史事实，见 [STATUS](STATUS.md)。

## 验证

- `cargo test --lib cache`：真实 `AppState` 下载配置的 600 秒 TTL/预算回归，确定性验证超过 60 秒仍保留、599.999 秒仍保留、600 秒精确失效且读取不续期；另含通用 TTL 边界、回收字节、LRU、条目/字节预算、权重溢出、10,000 次插入的持续有界性。
- `cargo test --test cache`：100 个同键等待者只运行一个 loader、32 个 OS 线程间共享、不同键并行、单等待者/全部等待者取消、失效代际竞态、失败共享与重试、超大值不保留、零配置边界、活动加载/键准入与 panic 后可重试。
