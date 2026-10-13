# 共享目录树：部署兼容与离线差分

## 范围与接入

`src/tree.rs` 只负责部署代码中真正共享的两件事：目录 BFS 遍历、name-keyed 嵌套树转换。天翼与移动使用同一个模块；HTTP、认证、客户端 clone、分页和列表响应归一化由各 provider/caller 负责。不新增 plugin、trait、第二层通用网络抽象，也不把移动解析类的目录分类规则混进共享 walker。

共享文件由主集成方接入，本任务不修改：

```toml
# Cargo.toml
futures-util = { version = "0.3", default-features = false, features = ["alloc", "async-await", "std"] }
serde_json = { version = "1", features = ["preserve_order"] }
```

```rust,ignore
// src/lib.rs
pub mod tree;

// caller：在此 clone reqwest Client/provider；模块不存储认证。
let walked = tree::walk(
    move |id| {
        let provider = provider.clone();
        async move { provider.list_all_pages(&id).await }
    },
    &root_id, &root_name, 6, 400,
).await;
let nested = tree::nested_tree(&walked, &root_id, &root_name);
let file = walked.file(&requested_id); // 与 allFiles.find 一样，首次发现优先
let response_fields = walked.to_json();
```

实际 callback 类型是 `Fn(String) -> Future<Output = Result<Vec<serde_json::Value>, E>>`，不要求 `Send`、`'static` 或固定错误类型。provider 应将非数组响应归一化为空 Vec；原函数会将非数组 list 当作 `[]`。失败结果同样成为空的预加载节点。

## 精确保留的常规行为

来源：当前 `.scratch/baseline/readable.js:14889` 的 `jxpanWalkDirTree`、`:14985` 的 `jxpanBuildNestedTree`。

- ID 优先级：`fileId || id || coID || contentID || ""`，再执行 JSON 域的 JS `String` 转换。空 ID 不记入 rows；数值 0 是 falsy，但字符串 `"0"` 是有效 ID。
- 名称优先级：`fileName || name || coName || contentName || ""`；大小优先级：`fileSize || size || coSize || contentSize || 0`。不擅自转整数或丢失选中字段的 JSON 类型。
- JS truthiness：空字符串、0、false、null 是 falsy；空数组、空对象和 `"false"`、`"0"` 是 truthy。
- walk 目录判定严格为 `isFolder || isDir || coType === 1`；`coType: "1"`、`coType: true`、`contentType: 1`、`type: "folder"` 单独出现不算目录。这与移动解析类规则不同。
- rootName 原样用于 walk 的初始 currentPath。没有 base 时 childPath 保留选中的名称类型；有 truthy base 时使用 JS 字符串拼接 `/`。因此非常规 JSON 名称的路径也不擅自归一化。
- 默认并发 6、maxNodes 400；输入 0 跟原函数的 `||` 一样选择默认值，而不是形成 0 大小批次死循环。正整数并发上限取 maxNodes，不影响可达请求序列。
- FIFO 队列、整批提前标记 seen、整批等待后按队列顺序处理结果。完成顺序不会改变 allFiles/allFolders/path。
- 重复 queued ID 仍然占用 admission 预算，但只发一次 list 请求。相同目录的首次路径优先；重复 file/folder rows 和 allFiles/allFolders 条目不被“优化”去掉。
- `seen.size + queue.length < maxNodes` 的入队条件原样保留；超预算目录仍保留 folder row，在 nested 中成为 `{}`。
- 空目录与 list 失败都生成 `folder_list`、`_preloaded: true`、实际 epoch 毫秒 `_preloadedAt`。
- nested 默认 startId 回落到 walk.rootId，默认 startName 是 `"root"`，不自动拿 rootName 替换。
- nested 先 folders 后 files，空名称回落到 ID；同名仅加一次 `#ID`。若后缀也重名，像部署一样覆盖，不循环生成新名称。
- 共享子树可在多个父节点下面展开；不使用全局 visited 来错误截断 DAG。
- 使用 `preserve_order` 加 JS array-index own-key 排序：数字索引键在前按数字排序，其余键按插入顺序。不能用默认 BTreeMap 的字典序代替。

## 安全边界：明确不是旧函数一致性

- 原 nested builder 对目录环会无限递归/栈溢出。本实现用显式 frame stack 与祖先集合；回到祖先的 edge 输出 `{}`。三个 cycle 向量仅对 **walk** 调用旧函数，绝不调用旧 nested，也不声称环的 nested 原函数差分通过。
- 极深目录树的展开深度最多 400；超过的目录 edge 输出 `{}`。构造 5,000 节点链测试显式迭代构建、边界和 JSON 序列化。默认 maxNodes 400 下的普通无环树不会因此额外丢失已访问节点。
- `__proto__` 是普通数据 key，不模拟原 JS `{}` 的 prototype setter。单独的 Rust 安全测试核对目录 ID、目录名、文件名均可安全保留；这项故意偏离原函数。
- API 的 ID/name/options 是 `&str`/`usize`；JS 任意非字符串根参数、负数/小数并发等不进入该类型化接口。JSON 列表字段仍保留 JS truthiness/coercion。
- 不使用无界 spawn；没有 await 期间持有的锁。每批 Future 数量有界，使用 `join_all` 保留输入结果顺序。

## 离线 oracle 与覆盖

`tools/tree-vectors.mjs` 使用 Acorn AST 的原始 source offsets，直接切出上述两个原函数，校验定义行号并在 VM 中执行原源码。没有手写 JS 替代实现；VM 无 fetch、网络、凭据、存储、定时器。listFn 只读取内存 graph，通过微任务延迟制造批次重叠和乱序完成。

固定来源与抽取证据：

- 部署 `_worker.js` SHA-256：`47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118`
- 两个精确函数拼接 SHA-256：`84d7da035abc17d7275a6e6bb410626c698be49ace4241169d67baa869dfbf56`
- oracle Date.now：`1704164645000`。Rust 测试先确认真实 `_preloadedAt` 为整数，再仅将该时间字段归一化比较。

当前 **139 个正常差分向量**（其中 **96 个 seeded DAG**），另有 **3 个 cycle walk 差分向量**。覆盖：空根/空目录、空/非数组/失败列表、Unicode 与斜线、四级字段优先与 falsy 回落、JSON truthiness/coercion、空根名时非字符串 childPath、独立于移动解析类的目录判定、同名/后缀覆盖、重复 ID、共享子树、numeric key 顺序、rootName/startName/subtree/missing 节点、并发 0/1/2/6/1000、maxNodes 0/1/2/4/7/400、重复队列占预算、450 宽目录的默认 400 限制、24 层正常链。测试比较 walk JSON、请求顺序、峰值并发、节点 key 顺序、nested JSON 和所有嵌套 object 的 key 顺序。

`tests/tree_vectors.rs` 每次运行实时调用本地 Node oracle，不依赖额外 fixtures 文件；因此需要本地私有 baseline 和 `npm --prefix tools ci` 已安装的 Acorn。路径导入 `src/tree.rs`，不要求本任务修改共享 lib.rs。

```sh
node tools/tree-vectors.mjs --summary
cargo test --test tree_vectors
```

## 本次实际验证状态

- `rustfmt --edition 2024 src/tree.rs tests/tree_vectors.rs` 成功。
- Node oracle 实际运行成功，报告 **139 normalVectors、3 cycleWalkVectors**；明确 `originalCycleNestedExecuted: false`。
- `cargo test --offline --test tree_vectors` 未进入编译：本机缓存缺少 `cbc`。普通 `cargo test --test tree_vectors` 在 120 秒超时，未拿到测试结果。
- 为提前检查 Rust 源码，使用现有 target 中的 serde_json/tokio/futures_util rlib 单独 rustc 编译测试二进制，编译成功。运行结果 **2 passed / 1 failed**：深链安全与 prototype-key 安全通过；139 个正常向量与 3 个 cycle walk 的全部 JSON 值、请求顺序、峰值并发及 safe nested 断言均完成，最后统一报告 **175 个 key-order 不一致**。原因是已有 serde_json rlib 未启用 `preserve_order`，第一个不一致为 `empty-root-name-json-path-types` 的节点顺序（实际字典序 `array,bool,number,object,r`，oracle 插入顺序 `r,array,number,object,bool`）。这是必需 feature，不能将现状称为 Rust 全部差分通过。
- 主集成方启用上述两个依赖配置并恢复可构建的依赖环境后，必须重新执行 Cargo 测试。未提交，未改共享配置/模块/路由。
