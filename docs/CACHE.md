# TTL 缓存与相同请求合并

模块：`jxpan_rust::cache::{BoundedTtlCache, CacheConfig, CacheError}`。该模块独立于 HTTP 和 provider，可分别创建解析结果缓存与下载链接缓存，在 `AppState` 中共享。克隆缓存共享条目与请求合并状态。

## 路由接入约定

- 解析与下载应使用不同的缓存实例、TTL 与字节预算。下载 TTL 必须保守，不能超过上游签名链接的实际有效期；未知有效期时使用很短的 TTL，或只合并、不保留。
- 键必须包含所有影响返回值的参数：provider、规范化后的分享标识、密码、文件 ID、完整目录链、具体下载方式、**有效凭据的作用域**。不要使用容易发生歧义的简单字符串拼接；可以使用 JSON 元组编码。不同凭据、密码或目录链不能共用条目。不要记录这些键。
- `get_or_init(key, load, cache_weight).await` 返回 `Result<Arc<V>, CacheError>`。闭包要拥有请求所需的参数和 client，不能借用临时 HTTP 请求。
- `cache_weight(&V) -> Option<usize>`：仅成功、完整且有效的上游结果返回 `Some(weight)`；失败、空链接、认证拒绝、非法下载 URL 等返回 `None`。并发等待者仍共享失败结果，后续请求重新加载。
- `KeyTooLarge` 应转换为受控的客户端参数错误；`InFlightLimit` 应转换为 429 或 503。不要在容量耗尽时无界绕过缓存调用上游。
- 下载 URL 应在进入缓存前验证协议、主机和 header 编码。

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

## 验证

- `cargo test --lib cache`：精确 TTL 边界、读取不续期、回收字节、LRU、条目/字节预算、权重溢出、10,000 次插入的持续有界性。
- `cargo test --test cache`：100 个同键等待者只运行一个 loader、32 个 OS 线程间共享、不同键并行、单等待者/全部等待者取消、失效代际竞态、失败共享与重试、超大值不保留、零配置边界、活动加载/键准入与 panic 后可重试。
