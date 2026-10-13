# 天翼只读 HTTP 内核

## 范围与接入

新增 `src/providers/cloud189_client.rs`，复用 `crate::providers::cloud189` 中已做差分的签名原文与哈希函数。构造：

```rust
let http = reqwest::Client::builder()
    .redirect(reqwest::redirect::Policy::none())
    .build()?;
let mut cloud = Cloud189Client::new(http, credentials);
```

调用者必须使用**禁止自动重定向**、不带全局凭据/共享 Cookie jar 的客户端。传入的 `reqwest::Client` 内部策略无法由此适配器重新配置；天翼 API 无须跟随下载/CDN 重定向。本层只获取直链字符串，不请求直链目标、更不下载文件正文。

凭据只读 snake_case：`session_key`、`session_secret`、`open_access_token`、`cookie_login_user`，可选 `share_session_cookies`。每个实例保存自己的 Cookie 状态；没有全局凭据、打印日志或 `Debug` 实现。`sson` 在这些原始方法中不发出，故不额外拼进请求。登录、凭据续期/失效标记、token 列表调度交由外部下一阶段处理。

主 agent 接入时须自行增加模块声明 `pub mod cloud189_client;`，及以下依赖（本次不修改共享文件）：

```toml
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
httpdate = "1"
uuid = { version = "1", features = ["v4"] }
```

沿用现有 `serde_json`、`url`、`tokio`。本实现不使用额外测试依赖、不要求 tokio `time` feature；外部重试/续期可能需要。不得关闭 TLS 验证。

## 接口与结果

四个方法均异步、使用 `&mut self` 维护实例 Cookie，返回 `Result`：

| 方法 | 成功业务结果 | 业务失败 |
| --- | --- | --- |
| `get_share_info(share_code)` | `Some(Value::Object)`，部署 camelCase 六字段 | `None` |
| `check_access_code(share_code, password)` | `{"success":true,"shareId":"…"}` | `{"success":false}` |
| `list_share_dir(share_id, folder_id, password, share_mode)` | `Some(Value::Array)`，包括空数组 | `None` |
| `download_url(file_id, share_id)` | `Some(Value::String)` | `None` |

`get_share_info`、`list_share_dir`、`download_url` 返回 `Result<Option<Value>, Cloud189Error>`；密码验证返回 `Result<Value, Cloud189Error>`。错误仅有静态 `operation`、`kind`，不携带 URL、凭据、密码、响应正文或 reqwest 原始错误文本。

网络、请求构造、超时、正文读取、JSON 解析错误不会伪装成业务 `null`。目录/直链回退会继续尝试：后续成功优先；全部失败且曾出现上述异常则返回最后一个明确错误；纯业务拒绝返回 `None`。这是有意区别于原 JavaScript 吞异常的部分。原部署有效 JSON 的非 2xx 响应仍解释为业务结果，本实现同样不先调用 `error_for_status()`。

## 部署合同

基准 `_worker.js` SHA-256：

`47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118`

精确提取类为 `.scratch/baseline/Cloud189Parser.js`，JSON 扫描器来自 `.scratch/baseline/readable.js`。向量工具验证 worker SHA 和类包含关系；不执行整个 worker。

### 分享信息 / 密码

- 分享信息请求固定 `https://api.cloud.189.cn/open/share/getShareInfoByCodeV2.action`。保留 `sessionKey`、浏览器 UA、Referer 和 noCache。
- 顶层 `res_code` 必须是数值 `0`，不将字符串 `"0"` 视为成功。`fileInfoAO.fileId` 备用分支保留原部署不检查其 res_code 的行为。
- 字段优先级保留 JS `||`；分享信息中的 `isFolder == 1`/`isDirectory == true` 与列表文件项的严格 `1`/`true` 区别保留。
- 密码验证固定 `POST /open/share/checkAccessCode.action`；正文使用 `encodeURIComponent` 语义而非 URLSearchParams（空格 `%20`，`!'()~` 不转义），含空 `validateCode=`。
- 分享信息与密码验证中 `COOKIE_LOGIN_USER` 提取刻意保留原方法 `split('=')[1]` 截断行为，而不是擅自修复。

### 目录

顺序为 **V1 → V3 → Open → App**：

1. V1 固定 `https://cloud.189.cn/api/open/share/listShareDir.action`，顺次尝试 `[share_mode, share_mode == "1" ? "2" : "1", "3"]`，包括输入 `"3"` 时末尾重复 `"3"`。每次 pageNum=1、pageSize=200，不新增分页。
2. V3 固定 `https://api.cloud.189.cn/open/share/listShareDir.action`，不新增无效 sessionKey 重试。
3. 有 Open token 才尝试 Open：原签名仅包含 AccessToken、Timestamp、fileId、shareId；认证参数放在 query，不改成 headers。
4. 有 sessionKey + sessionSecret 才尝试 App：HMAC-SHA1，Date 为 HTTP GMT 格式，签名路径不包含 query；保留 TELEPC/version=6.2、rand、UUID、desktop UA。

`res_code` 在目录解析中同时接受 `0` 与 `"0"`。**`Some([])` 是成功并立即停止回退；`None` 才继续。** 文件列表先规范化 `fileListAO`；仅当所得列表为空时再读顶层列表。文件大小用 `size ?? fileSize ?? 0`，不把合法 `0` 覆盖掉。

### Cookie

- 提取多条 Set-Cookie 的首个 `name=value`；排除空值、Max-Age=0/-1、HTTP Expires 已过期的记录；无可用记录才尝试原类 comma-split 后备分支。
- 密码验证获取到 Cookie 时替换分享会话串；目录获取到 Cookie 时只在分享会话尚为空时初始化，不改成无条件合并。
- V1 优先 COOKIE_LOGIN_USER 再分享会话；V3 优先分享会话再 COOKIE_LOGIN_USER；Open/App 只携带分享会话。
- `merge_cookies(existing, incoming)` 单独提供原类按 Cookie 名覆写、保留插入顺序的合并操作，不擅自改变上述网络方法的保存规则。Cookie 值中的 `=` 保留。

### 直链

只读组合顺序：Open → App(dt=1 + shareId) → App(dt=3 + flag=1)。没有 shareId 时跳过 App dt=1；缺凭据时跳过对应方案。Open 有 errorCode 则拒绝该响应，即使其中有 fileDownloadUrl；App 按原方法仅检查 fileDownloadUrl。

Open 的 Java-MD5 去掉前导零；签名原文使用 pure 模块，不排序/编码。App 使用 pure 模块 HMAC-SHA1；HTTP Date 及 path 与签名原文完全一致。下载链接只替换**第一次** `&amp;`，不以 `http` 开头则加 `https:`；不更改原部署的宽松 URL 返回规则。直链是数据，不作为下一跳请求地址。

分享信息和 Open 直链请求保留原方法 query 直接拼接；其他查询使用 URLSearchParams/form 编码。这保留原部署对特殊字符的行为，但调用者应传真实分享码/ID，不依赖此行为注入参数。域和 path 始终由固定常量确定。

JSON 使用部署词法扫描器：未加引号的整数达到 16 位即先转字符串再 JSON 解析，避免大 ID/size 精度损失；字符串内数字不改。每次请求超时 20 秒，响应 JSON 上限 2 MiB，超限明确报错。只读 API 不读取网盘文件正文。

## 离线差分测试

```sh
node --check tools/cloud189-http-vectors.mjs
node tools/cloud189-http-vectors.mjs  # 输出 synthetic-only JSON 向量
cargo test --test cloud189_http
```

`tools/cloud189-http-vectors.mjs` 在隔离 VM 中执行**原类网络方法和原 jxpanParseJsonSafe**。fetch 是严格内存 Mock，限定天翼六个只读 path、HTTPS 和有限响应队列；原部署的日志静默，续期 hook 明确返回 null，转存/删除从不执行。没有手写期望请求/输出冒充差分。

`tests/cloud189_http.rs` 将同一响应 transcript 提供给单线程 loopback stub，再比较实际 Rust 方法输出，以及线上将发出的 method、完整 path/query、所有部署 headers、POST body。随机值/时钟仅在私有 cfg(test) loopback 构造器中固定；生产构造器不能注入 endpoint。私有入口仅接受 HTTP 数字 loopback IP，不接受其他主机。

覆盖四层回退与 V1 模式顺序、empty-vs-null、Cookie 捕获/优先级/重复值、大小 nullish 规则、大 ID、嵌套分享、严格/宽松 res_code、POST 编码、签名、App 两种 dt、非 2xx 有效 JSON、非法 JSON 以及脱敏 transport error。

当前交付时：Node 语法和 VM 生成已通过（23 场景 / 56 请求 / 3 合并向量）；Rust 差分运行被共享 Cargo.toml 尚缺 reqwest/httpdate/uuid 阻塞，不能将其描述成已通过。接入者加约定依赖后必须执行上面的 cargo 测试；本次没有访问真实网盘、读取真实秘密或修改共享清单。

## 明确不实现

不实现/调用转存、删除、扫码登录、Open token 获取/续期、ensureCookieLoginUser 网页引导、存储/鉴权、路由、树缓存/递归树、自动分页、token pool 调度，以及整文件下载。真实网络核验由主 agent 负责；离线合同测试不能证明真实服务可用性。
