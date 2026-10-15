# 天翼隔离目录 / 自有分页能力（官方 OAuth REST）

## 状态与适用边界

`src/providers/cloud189_transfer_client.rs` 提供生产 HTTPS transport、官方 OAuth REST 请求、fresh-only provision 回调、自有目录分页及完整树遍历，已由 `providers` / 根 lib 导出，并由根 crate 的 `tests/cloud189_transfer_client.rs` 测试。**模块导出不等于业务启用**：`enable_owned_read` / `enable_provision` 默认均为 false，当前 AppState/router 没有实例化或接入该客户端；公共天翼解析不调用它，也不自动创建目录或转存。

这是独立官方 REST 能力，不是冻结版 `Cloud189Parser.js` 已有方法的移植，也不宣称冻结版具有 `createFolder` / 自有列表能力。不臆造 `/api/open/file/listFiles.action`、WebAPI 创建接口、`hasMore`、`recordCount` 或 share-list 分页。另一个已导出的 `cloud189_transfer::TransferKernel` 使用独立 `enable_transfers`（默认 false）；开启 owned 的两个开关不会隐式开启 SHARE_SAVE，更不会使公共下载获得 owned 回退。

现有官方文档证据要求 **AccessToken + AppSecret**，不是扫码授权快照中的 **SessionKey + SessionSecret**。客户端使用独立 `OAuthCredentials`，无账户字段自动转换、无登录/扫码、无 SessionKey fallback。没有真实调用证明这两个 REST 端点支持 App session 凭据；该认证变体明确未实现。仅持有 QR/session 凭据时应保持能力不可用，不能把 session_key 当 OAuth token、session_secret 当 AppSecret。

OAuth principal 与 AuthManager QR/Session principal 的**同账户身份绑定未实现、未验证**。共享 AppState、成功保存授权或 lib 导出都不能建立这项绑定；caller 声称相同或 ID/name 相似也不是证明。组合两套协议前必须有可信同账户绑定，否则停在未启用边界。本客户端的 OWNED 仅指 OAuth 授权下返回的自有列表，不能证明另一个 session 的所有权。

## 已有官方文档证据与逐级链接

以下官方页面及页面引用脚本是现有协议依据；本轮仅核对当前代码和更新文档，未联网重新获取页面，也未做真实账户验证：

| 来源 / 锚点 | 核验事实 |
| --- | --- |
| https://id.189.cn/html/api_detail_121.html#5 | 原始官方目录明确链接 `api_detail_135.html` → `http://api.cloud.189.cn/listFiles.action`，`api_detail_147.html` → `http://api.cloud.189.cn/createFolder.action`。接口不是“不存在”。 |
| https://id.189.cn/html/api_detail_121.html#3 | `Signature=hmac_sha1("AccessToken=…&Operate=…&RequestURI=…&Date=…", AppSecret)`；Operate 为 HTTP 方法，RequestURI 为资源路径。 |
| https://id.189.cn/html/api_detail_135.html#request-type | GET；支持格式 **xml**，不是未经核验的 JSON 协议。 |
| https://id.189.cn/html/api_detail_135.html#request-headers | 必填 AccessToken / Signature / Date。 |
| https://id.189.cn/html/api_detail_135.html#request-params | `folderId` (Long)、`fileType` 0=不限、`mediaType` 0=不限、`mediaAttr` 0=不获取、`iconOption` 0=不获取、`orderBy=filename`、`descending=false`、`pageNum` **从 1 开始**、`pageSize`。 |
| https://id.189.cn/html/api_detail_135.html#response-params | `listFiles` → `lastRev` + `fileList`；`fileList.count` 是 **子文件夹和子文件总数量**；`folder` 和 `file` 容器各 0..多个。 |
| https://id.189.cn/html/api_detail_147.html#request-type | POST；支持 xml。 |
| https://id.189.cn/html/api_detail_147.html#request-params | `parentFolderId`、可选 `relativePath`、必填 `folderName`。本实现只发送显式父 ID 和单层 UUID 名称，绝不发送多层 relativePath，也不落入“空父 ID=默认目录”。 |
| https://id.189.cn/html/api_detail_147.html#response-params | 根 `folder` 下 `id` (Long)、`name`、`createDate`、`rev`。 |
| https://id.189.cn/html/api_detail_137.html#response-params | 官方 folder 信息字段 `id` / `name` / `path` / `createDate` / `lastOpTime` / `rev`。 |
| https://id.189.cn/html/api_detail_139.html#response-params | 官方 file 信息字段 `id` / `parentFolderId` / `name` / `size` / `path` / `md5` / `createDate` / `lastOpTime` / `mediaType` / `rev`。 |
| https://id.189.cn/source/js/plus/openportal/api/iframe-child.js | 实际 GET 脚本为 iframe 高度 / 目录显示及导航逻辑，不包含可补齐 REST wire schema 的隐藏协议。 |

文档质量限制，必须保留：

- 官方 URL 使用旧 `http://`；生产实现**只**使用相同官方主机和路径的 `https://`，不降级、不跟随跳转。HTTPS 实际业务调用兼容性未做真实账户验证。
- 创建页 `#request-demo` 的请求行错误写成 `POST /getFileDownloadUrl.action?fileId=81`，与该页 URL / 标题及目录矛盾。实现以目录 + URL + POST + 参数表为准，**不照抄错误样例**。
- 正确返回样例的 HTML 已丢失 XML 标签，仅剩拼接文本，不能声称样例给出了完整 wire fixture。因此测试 XML 全为 **synthetic**，根据官方表格的容器及字段构造；不是录制的线上响应。
- 列表页没有单独展开每个 folder/file 子容器字段；字段名依据同站官方 folder/file 信息表，严格接收公共字段 `id` / `name`，file 必须有 `size`。若实际列表不是这些字段名或有额外结构，则 **Err**，不猜别名、不自动降级。此 schema 一致性和实际可用性尚未 live 验证。
- folder 信息/创建表没有 `parentFolderId`。父目录证明来自官方定义的 **listFiles(folderId) 直接子项上下文**，不是伪造响应中的该字段；若响应额外提供官方 file 表中的 parentFolderId，也必须与请求父 ID 一致。

## 签名与生产 transport

`oauth_signing_text` 复用 `cloud189::app_signing_text(token, method, path, date)` 的精确顺序，再仅将开头字段名 `SessionKey=` 替换成官方规定的 `AccessToken=`；随后复用 `cloud189::hmac_sha1(AppSecret, text)`。没有重复实现密码学。签名路径不包含 query 或 body；方法保持 GET / POST 大写；Date 由现有 `httpdate` 生成。

`ReqwestTransport::new()` 自己创建 reqwest 池，而不是信任调用者传入不可检查的 client。transport 可 Clone，复用同一连接池：

- `Policy::none()`，`https_only(true)`，`retry::never()`；没有 cookie jar、默认账户头或 Debug 输出。
- 只允许 `api.cloud.189.cn` 的 GET `/listFiles.action`、POST `/createFolder.action`；拒绝其它主机、非 HTTPS、额外端口、userinfo、fragment、其它路径和方法。
- 每个请求包含完整 body 流读取 **20 秒**总截止，最大 **2 MiB**；检查 Content-Length，并对实际每块累积大小检查。mock transport 经同样的外围超时、状态码、2 MiB 与 XML 检查。
- HTTP 非 2xx（包括重定向）为 Err，不处理 Location，不请求下载地址。
- 有限 XML 子集 parser：禁止 DTD / 自定义实体 / 外部实体 / namespace / attributes / CDATA / comments；支持标准五种实体、数字字符引用、有限 UTF-8 XML declaration、空元素。深度 ≤12、元素 ≤20000；不支持的合法 XML 同样明确 Err，不假装成功。
- 错误为静态 enum，无响应内容、凭据、账号 ID 或敏感 URL 的日志输出。

## Provision 安全边界

构造时要求显式授权的正 Long root（拒绝 `-11` / `-12` / `0` / 空 / 非数字 / 非规范 Long）。`enable_owned_read` 和 **独立** `enable_provision` 均为 true 才能创建。回调再核对 root 与构造授权 root 完全相等，并只接受 `jxpan-transfer-<canonical UUID v4>`。

步骤严格串行：

1. 完整分页列授权父目录；同名的文件或文件夹均立即 Err，不 find-or-reuse。
2. POST 创建 **一次**，没有自动 retry。名称在尝试前被记入 instance-local attempted set，任何失败（包括不明响应）都会烧掉该名称，再调用仍 Err。
3. 创建响应必须为 folder，ID 为真实正 Long，不是 root / 先前任何子项 ID，名称精确一致。
4. 重新完整列父目录，恰好一条同名项，必须为 OWNED folder，ID、name、请求 parent 上下文全部匹配。
5. 完整列新 child，必须 count=0 / 无子项，之后才将 ID 加入本 client 的 verified scopes，并返回它。

任何 POST 后错误都可能留下未核验的目录；交由手工恢复，**不重试、不换名补偿、不自动删除**。实现没有 delete/transfer 请求。UUID + precheck 不是服务端 compare-and-create 锁，无法证明并发写者从未插入或瞬时复用同名目录；parent 不应并发写这个隔离父目录，且不能把这次快照理解为永久空目录保证。

## 官方字段驱动的分页 / tree::walk

- 页大小固定 100，pageNum 从 1 递增。结束条件：已累计的唯一 item 数 **等于**官方 `fileList.count` 总数，而不是编造 hasMore 或“空页默认完成”。count=0 必须没有子项。
- 每页必须恰好有 `min(100, count - 已累计数)` 项；短页、空中间页、过多项、重复 ID、同一列表里重复页都 Err。count 或 lastRev 在页间改变立即 Err。
- 总资源 ≤1000。遍历时剩余全局资源和目录预算传入分页，知道超限就 Err，不继续抓更多页。
- 仅允许本 client 此次 provision 已核验的 child 为树根，不能直接 traverse 任意持久化 ID / 分享 ID / 授权父根。
- 重用 `tree::walk(..., concurrency=1, max_nodes=400)`，独立共享状态记录第一个 listing Err、全部资源 ID 和数量。树 walker 的 catch-and-empty 不会吞掉最终错误；全局重复 ID、祖先循环、root 回边、>400 nodes（含根）、>1000 resources 均 Err。
- 最后核对 `nodes.len == visited == 已发现目录数` 和资源数量一致，避免 max_nodes 静默截断被当成功。不调用 sharelist。
- parentId 是每个已核验的 OWNED 直接子列表上下文；输出使用 kernel 接受的 `fileId/fileName/isFolder/fileSize/parentId`。

注意：当前 `TransferKernel::provision_scope` 对整个 callback 还有一个 20 秒外围限制，四个及以上顺序 HTTP 操作不能保证在该时间内完成。超时绝不能自动重试此 callback。kernel 的账户凭据检查也不等于 OAuth 认证通过；后续受控接线必须显式处理认证、同账户绑定和超时边界，不能假装已有 session 自动可用。

## 根 crate 离线验证与剩余接线

`tests/cloud189_transfer_client.rs` 已直接使用 `jxpan_rust::providers::cloud189_transfer_client`，不是 path module 或未接线源码的独立替身。所有 HTTP response 都来自 memory synthetic fixtures；production transport 测试只 build client / 检查 target，**不发网盘请求**。覆盖默认关闭、非法 root、UUID/root 越界、同名拒绝、POST 不明失败不重试、父目录 ID/name/parent 核对、非空 child、签名参数与头、总数分页、变更/重复/短页、HTTP/XML/2 MiB 错误、吞错防护、循环、node/resource 预算。

标准命令：

```sh
cargo test --locked --offline --test cloud189_transfer_client
```

根 crate 已通过 15 项 synthetic 回归，包括 node=400 完整遍历和 node>400 拒绝的双边界；调用真实 `cloud189` 的签名/HMAC 原语及 `tree::walk`，不替换为成功 stub。root lib 导出和测试接入已完成，不再需要为先前路由编译中间态绕开根 crate；离线通过不代表官方 HTTPS、OAuth 或 XML schema 已被真实账户接受。

后续受控业务接线（仍未完成）：

- 显式配置独立 OAuth credentials、授权 root 与两个开关，并先完成可信同账户绑定；缺少条件就返回未启用/能力不可用，不能从公共 query 接收 root、账号凭据或创建/转存授权。
- `provision_new_empty(&root, &name)` 适配 `provision_scope` 回调；两边 Error 都是实际错误 enum，需明确映射到 kernel 的公开 `TransferError`，不能使用其 private Result alias，也不能吞错返回空 ID。
- `walk_owned(scope_id)` 返回完整 Walk 后交给 kernel；失败原样传播，不用空树占位。
- `ReqwestTransport` 只涵盖本能力的两个端点，不能替代 share-save/download/delete transport；没有新增“stub 成功”的未实现方法。

未做真实扫码、登录、创建、转存、成功天翼下载或删除，也未验证端到端性能。公开分享下载已限制为 Open / AppShared；本客户端和 transfer kernel 虽已导出，仍未进入公共解析或自动任务。业务启用需要另外显式授权后的官方 OAuth/HTTPS/schema 验证、可信同账户绑定及受控 kernel 接线，不能把离线测试通过当作线上操作已成功。
