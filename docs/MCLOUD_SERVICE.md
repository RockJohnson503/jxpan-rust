# 移动云盘只读业务编排与离线差分

## 范围与接入

`src/providers/mcloud_service.rs` 复用现有 `mcloud`、`MobileCloudClient` 与共享 `tree::{walk,nested_tree,Walk}`。不实现登录、续期、转存、删除、凭证持久化或缓存。只有列目录及请求下载地址两种只读网络操作；下载地址作为 JSON 数据返回，不访问下载地址。

为遵守本任务“只新增四个文件，不改 shared 文件”的约束，**尚未修改 `src/providers/mod.rs`，未接入 HTTP 路由**。集成测试通过 `#[path]` 编译同一份业务实现。主代理接入时需自行注册模块。

生产入口：

```rust
pub struct ServiceConfig {
    pub enabled: bool,
    pub authorization_configured: bool,
}

pub async fn parse(
    client: &MobileCloudClient,
    config: ServiceConfig,
    share_url: &str,
    pwd: &str,
    fid_chain: &[String],
    file_id: &str,
) -> serde_json::Value;

pub async fn parse_with_fid_chain(
    client: &MobileCloudClient,
    link_id: &str,
    pwd: &str,
    fid_chain: &[String],
    file_id: &str,
) -> serde_json::Value;
```

现有客户端凭证字段私有，也没有 enabled 配置，因此 `parse` 显式接收两个配置门禁布尔值，不重复传递或保存凭证。调用者必须从同一份配置产生这两个布尔值，`authorization_configured` 对应原类的 authorization 非空判断，**不是认证是否成功**。禁用优先返回 503，未配置返回 401，再校验分享链接。已配置但认证/列表请求失败按原行为表现为分享为空的 404，不改成 401。

`parse_with_fid_chain` 接收已提取的 link ID，和原类方法一样不自行执行 enabled/auth/link 门禁；它是专门的子目录 action，不是 `parse` 的 fid 分支。接入路由需在适当位置做配置门禁。

客户端必须使用正常 TLS 验证和 `reqwest::redirect::Policy::none()` 或固定主机的重定向策略。业务层不增加 endpoint setter，不将输入分享 URL 用作 HTTP 请求地址。客户端限制固定主机的责任仍在现有 `MobileCloudClient`。

## 返回行为

- 根目录只有一个文件：返回 `folder: []`、`list: [...]`、`tree_complete: true` 等原类完整字段，`download_url: null`，不预取下载地址。
- 多文件/包含文件夹：4 并发、400 节点的共享 walker，返回 `data.tree` / `data.sid`；采用共享 walker 自身的字段优先级和分类，不用移动解析器分类替换它。
- `parse` 的 fid：只使用最后一个 fid 定位已经遍历的 tree。缺失 fid 保留原类特殊行为——节点可回退 root，但 nested tree 仍按请求 ID 构建。
- `parse` 的 file ID：优先于 fid；从 root 顺序 DFS，深度 0–8 可访问，深度 9 不访问；visited 防重复/循环，第一次匹配获胜。
- 文件 ID 实际是目录：分别保留普通 file-ID 与子目录 action 的两个不同 400 文案。
- 单文件详情只使用 `path || coPath` 获取下载地址；没有路径不以 ID 补请求；下载失败返回 `download_url: null`。
- `parse_with_fid_chain` 逐项加载 fid，支持完整目录列表、父目录字段、层级失败、文件夹误作文件、未命中文案及 `available_files` 中 undefined 字段省略。
- filesize 遵循原 `formatFileSize` 的 parseInt / 单位 / 两位小数；无效数值为 `NaN B`；整数前缀、hex、科学计数法阈值及精确 halfway 向上舍入已纳入差分。
- 传输错误被折叠为原类的空列表或 null；不输出原始错误、token、密码、URL 正文或响应正文。用户提供的文件/目录 ID 和原类 `available_files` 仍保留。

## 根目录请求优化：有数据的等价边界

原类先获取根目录判断单文件/目录，再在 `parseMcloudTree` 内重复获取 root。本实现把首个 root 列表交给 walker callback，其他目录仍按 walker 请求；不跨请求缓存，不锁 await，不 spawn。

离线 oracle **先执行未经改写的精确原类与共享 walker**，再用相同内存 graph 做首个 root 列表复用实验，断言完整返回 JSON 相同且每个受影响案例只减少一个列表调用。125 个案例中有 **22 个案例各节省 1 次 root 请求**。420 个直接子目录的上限案例：原类 401 次列表调用，本实现 400 次；本实现最高同时 4 个 callback，完整 JSON 相同。

等价前提是同一解析期间目录内容稳定；若远端两次 root 响应不同，复用首次快照可能与原类第二次快照不同。本任务没有真实帐号，未测真实延迟或远端变化场景。

## 离线 oracle 与验证

`tools/mcloud-service-vectors.mjs`：

1. 校验 `.scratch/baseline/_worker.js` SHA-256 为
   `47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118`。
2. 从 `readable.js` AST 提取精确 `MobileCloudParser`、`formatFileSize`、`jxpanWalkDirTree` 和 `jxpanBuildNestedTree`；校验提取的 class 与 `MobileCloudParser.js` 字节一致（忽略文件末尾空白）。
3. VM 的 fetch 是严格拒绝 stub；没有真实帐号、网络、文件写回或计时任务；console 被静音，原类私密日志不会被打印。原类网络方法替换为 synthetic 内存 list/download callback，仅验证业务编排，不冒充 HTTP 协议测试。
4. 原类缓存 get 固定 miss，set 无操作；固定 Date。生成 JSON 只写 stdout，不落盘 fixture。

原 class hash：`4c2784a32492b016548fcbf6286a71fd2ca79b995280aeb3eff8d45bba680661`。
组合源码 hash：`fd47bc886e26eb579830d32abc137cd4c1c90c6c67b5671c769c40649cda190b`。

`tests/mcloud_service.rs` 每次运行实时执行原源码 oracle，无旧 fixture 回退；比对每个案例的完整 JSON、列表调用顺序、DFS 顺序、下载 callback 参数与调用次数；用异步 poll callback 验证并发上限和所有回调完成。

```sh
node tools/mcloud-service-vectors.mjs --summary
cargo test --test mcloud_service -- --nocapture
```

需要 Node、`tools/node_modules/acorn` 及本地私有 `.scratch/baseline`。未提供 baseline 的环境会明确失败，不会跳过后声称通过。

本次执行结果：**125 个原类离线 JSON 案例通过，2 个 Rust 测试通过**。覆盖多层/空/单文件、未配置/禁用/无效链接、认证列表失败、错误 file ID/目录、重复名、fid 链、字段优先级、path/coPath、下载失败、深度 8/9 边界、visited 循环、子节点失败、400 节点上限及 32 组固定种子 DAG（分别测 tree 和 file ID）。

## 明确未覆盖与接入注意

- 不声称全输入域等价：尚未差分所有 malformed/null/primitive 列表行、特殊对象键（尤其 `__proto__`）、超长 hex 数值和任意 JS 非字符串字段。共享 walker 对循环 nested tree 的安全行为不是原递归实现的无限递归行为；这里只对 file-ID DFS 执行循环案例。
- 未执行原类缓存 hit 分支；业务层按要求无缓存，TTL 应由主代理单独实现。
- 未测试真实云盘、真实认证、HTTP/AES 协议、登录/续期。生产 adapter 本次编译验证，实际业务通过同一核心的 callback seam 差分验证。
- 原 `parse` 在 authorization 非空但解码账号为空时会设 `account = "default"`。当前 `MobileCloudClient` 的私有 account 在这种情况下返回空列表，业务层无权限修正；普通 synthetic 非空账号覆盖通过，但**这项生产适配差异需主代理在共享客户端层决定如何处理**。
- 无任意 URL setter、持久化或业务缓存；模块注册、路由和有界 TTL 缓存不属于这四文件的修改范围。
