# 天翼转存：冻结协议、隔离回执与副作用边界

## 范围和完成程度

`src/providers/cloud189_transfer.rs` 已由根 lib/providers 导出，`tests/cloud189_transfer.rs` 直接测试导出的模块。生产可传入共享 reqwest Client，Options 默认关闭；AppState/router 尚未实例化或接入转存，未执行任何真实网盘写入。内存 transport 和原 JS mock fetch 的离线通过，不是登录、转存、定位、下载、清理的真人闭环。

已实现：确认存在的 SHARE_SAVE 批量请求结构、严格任务状态判定、有限串行轮询、单文件/批量转存编排、成功后定位实际自有文件 ID、绑定回执的文件清理、部分清理进度、下载失败仍清理。批量目录使用已有 `crate::tree::walk`，不写第二套目录 walker。

明确未实现/不得声称已验证：冻结类没有 `createFolder`、`batchSaveShare`、`checkTaskStatus`、`move`、`moveFile` 方法，无法从该版本恢复这些不存在的接口。Rust 的 `batch_save_share`/`check_task_status` 是确认的 `saveShareFile`/`checkBatchTask` 的安全编排名称，**不是发现了同名部署方法**。创建隔离目录和完整自有目录分页列表由 caller 提供可信能力；没有猜造 createFolder/move HTTP 请求。`isFolder:1` 与多项 `taskInfos` 是现有数组 schema 的扩展，仅内存测试，不是服务端已验证的目录转存保证。

## 冻结证据与真实调用路径

研究基线 `.scratch/baseline/_worker.js` SHA-256：

`47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118`

Oracle 从同一 `readable.js` AST 提取**整个 Cloud189Parser 原类**及原 `jxpanParseJsonSafe`，未重写 saveAndDownload 等业务代码。核验存在/不存在的方法，并拒绝部署哈希变化。

| 源码位置 | 行为/真实调用链 |
|---|---|
| `readable.js:13046` | getWebSessionKey：Web user brief + MD5/Cookie；读取 sessionKey |
| `readable.js:13075` | getRootDirectory：Web getRootDirectory，可带 sessionkey；不是 saveAndDownload 的实际依赖 |
| `readable.js:13109` | saveShareFile：POST Web createBatchTask，type=SHARE_SAVE，taskInfos 数组，targetFolderId/shareId |
| `readable.js:13152` | checkBatchTask：POST Web checkBatchTask；仅数字 res_code=0，数字 taskStatus=3 且 errorCode 为 JS falsy 时完成 |
| `readable.js:13205` | saveAndDownload(shareId,fileId,fileName)：Cookie → webSessionKey → 固定 -11 → SHARE_SAVE → 最多15次、先等2秒 → App自有下载 → 仅拿到URL后deleteFile |
| `readable.js:13263` | deleteFile：POST App batchDeleteFile，form fileIdList，HMAC-SHA1 Operate=POST |
| `readable.js:13503` | 常规 parse 单文件：Open → 转存 → App；转存参数顺序正确 |
| `readable.js:14473` | resolveItemInFolder：saveAndDownload(targetFileId,shareId,name)，**参数倒置**，不能照搬 |
| `readable.js:16764` | parse_cloud189_file：Open → App → 转存；同样 fileId/shareId 倒置 |
| `readable.js:25113` | 路由匹配文件：Open/App后尝试转存；同样 fileId/shareId 倒置 |

天翼播放快路径 `readable.js:14697` 之后是 App → Open，明确不转存；不要将本模块无条件插入 fast down。移动云盘的转存协议不在本模块范围，不能把别的 provider 的 createFolder/move/任务接口套给天翼。既有两-provider路由顺序由主代理继续核验/集成，本模块不改它们。

## 确认的协议

- Web 基址 `https://cloud.189.cn/api`。
- App 基址 `https://api.cloud.189.cn`。
- Web 签名调用现有 `cloud189::web_signing_text(timestamp)` 和固定宽度 `cloud189::md5`，原文 `600100422{timestamp}1`。携带 appkey、sign-type、signature、timestamp、COOKIE_LOGIN_USER/可选SSON，保留部署 UA/Referer。
- SHARE_SAVE form 字段顺序：type、taskInfos、targetFolderId、shareId。单项 `{fileId,fileName,isFolder:0}`；URL form UTF-8/空格/特殊字符编码已与原 VM 逐字节比对。
- 检查任务 form 字段顺序：taskId、type=SHARE_SAVE。字符串 `"0"` 不当作数字0；字符串 `"3"` 不当作完成；errorCode优先判失败。失败/拒绝/transport异常立即停止，不盲重试。
- 删除调用现有 `cloud189::app_signing_text(...,"POST","/batchDeleteFile.action",date)` 和 `hmac_sha1`，没有第二套 Signer，也无 AES/RSA 实现。后缀 TELEPC/6.2/channelId/rand，body `fileIdList=<receipt文件ID>`。
- 响应使用现有 `cloud189_client::parse_json_safe`，保留大整数 ID。每次 HTTP/读body整体最多20秒，body上限2MiB；响应重定向拒绝。

Oracle录得的合成向量签名/编码在 `tests/fixtures/cloud189-transfer.json` 的 `wire` 字段；不是自写 Rust 签名器产生的预期值。

## 显式授权和隔离目录能力

默认 `Options::default().enable_transfers == false`；不开启时，不执行 provisioning callback 或任何请求。开启还须现有 Cookie + session pair。授权测试根目录由 `provision_scope(authorized_test_root, callback)` **显式传入**，禁止默认账户根 -11/-12/0、空ID、ID分隔符、返回授权根本身等。

Provision callback 收到精确的 `jxpan-transfer-<本次v4 UUID>` 名称。调用者必须实现以下可信约定：

1. 测试根目录确由该账号/该用户授权；普通字符串不是服务端权限证明。
2. 创建**新的空子目录**，而非 find-or-create、复用同名旧目录或返回任意已有目录；须验证返回的父目录和名称与请求一致。
3. 不与其他任务或外部写入者共用该子目录，不将账户默认目录传作子目录。
4. 创建前记录恢复位置；callback取消/超时/服务端创建成功但响应丢失等情况须自行安排人工核对，不能宣称无副作用。

`IsolatedScope`、`Receipt` 字段私有，不能任意构造、Clone、deserialize；绑定 kernel owner，scope消耗一次。Receipt含目录/taskID、本次请求项、实际资源ID、剩余删除文件ID、任务完成/文件清理标志；不含凭据。没有默认收藏目录枚举或清空接口。

**重要限制：冻结部署没有创建子目录/完整自有分页能力。独立官方 OAuth REST 客户端现已实现并导出，但仅 synthetic 验证且未接 kernel；与当前 QR/session 的可信同账户绑定未实现/验证，不能按凭据相似直接合并。没有已验证能力时保持关闭，不退回 -11。** 见 [owned 能力与认证边界](CLOUD189_TRANSFER_CAPABILITIES.md)。

## 成功后的定位、批量遍历和清理

`save_and_download` 委托 `batch_save_share`，顺序如下：

1. 校验开关、scope、shareId、本次项数量1..100、唯一名称。
2. 获取 Web session key；创建 SHARE_SAVE 任务，保存 taskId。
3. 串行 poll，最多15次，delay最多2秒（runtime调用最多3秒）；每次网络最多20秒。无spawn、无后台daemon、无无限poll。
4. 调用 caller 的完整、分页、**自有隔离目录** listing callback，返回 `crate::tree::Walk`。Callback应复用 `tree::walk(...,concurrency=1,max_nodes=400)`，不得用 share目录或旧缓存代替。最多20秒、总资源最多1000项。
5. 核对root=scope、每个发现目录均有node、资源ID不重复、不等于scope/授权根/默认根、每条parent链有限且最终到scope、本次顶层名称/类型唯一匹配。不能把请求的分享fileId当成转存后的ID，也不能靠模糊名称挑一个结果。
6. 对完整树的**所有叶文件**逐个调用现有自有下载 API callback（应调用 `Cloud189Client::download_app(id,"",false)`）；每次20秒，不请求下载体，不做App/Open签名副本。
7. 无论下载返回None/Err/超时，只要已有成功任务且资源可绑定，都执行 receipt文件ID清理。只有数字res_code=0才确认该文件删除，逐项保存已确认进度。显式重试不重复删除已确认项。

`tree::walk`会吞掉listing错误且max_nodes会截断，因此 callback须独立传播分页/读取失败、达到节点限额或未遍历完等错误。结构校验不是“服务器完整性证明”。本模块不重复实现目录walk；callback不满足约定时绝不能当成可靠receipt。仅从名字不能证明账号权限或服务端复制结果，本次隔离目录/完整列表/独占写入的可信约定缺一不可。

### 清理状态不能误报

- `Cleanup::FilesRemovedDirectoriesRetained`：仅本次绑定的**文件**全部得到删除确认；所有目录骨架（包括UUID子目录和转存目录）明确保留。Receipt的 `files_cleaned()`只表示文件清理，不表示目录删除或账户整洁。
- `Cleanup::ManualInterventionRequired`：任务结果不明、poll到限、listing/绑定失败、删除失败等。Outcome仍含receipt和可能已有下载链接；不能因为有URL就声称“清理成功”。
- 不递归删除目录，以防snapshot之后另一个写入者加入未在receipt里的文件。需要删空目录的能力应另行核验非递归/仅空删除协议并授权；这里未猜造它。
- Pending task到限不贸然删目标目录，可能仍有晚到转存。保留taskID/目录恢复信息并提示人工处理。外部取消未来、进程退出、panic不构成异步finally保证；caller须保护任务生命周期并记录恢复信息。没有跨进程自动持久化或自动重放删除列表。

## 与旧 JS 的有意差异

原 JS写固定 -11，下载/删除使用传入分享fileId，下载失败或poll到限不清理；delete失败仍返回URL。Rust不照搬这些风险，且不照搬任何“默认收藏目录全部清空”的旧版本行为。**本次冻结deleteFile实际只传一个fileId，未发现默认收藏目录清空逻辑；不能把不存在的行为写成此次基线事实。**

Rust只在新UUID隔离scope中绑定实际fileID，下载失败仍尝试文件清理；无法绑定或删除失败显式manual。出于非递归删除安全要求，目录保留是明示的策略差异，不可宣称完全清理所有网盘资源。

## 当前接线与剩余要求

1. `src/providers/mod.rs` 已导出 `cloud189_transfer`，root lib 可用；根测试不再以 path 替身绕开 crate。模块导出不是功能开关，公共路由仍不调用 kernel。
2. 从现有caller账号state传 `Credentials::from_account`，共享 `reqwest::Client`传 `ReqwestTransport::from_no_redirect_client`。Client必须已配置 `redirect::Policy::none()`、无cookie jar、无默认敏感headers；reqwest无法从传入Client反查这些设置。不能从未配置的默认Client克隆后假定安全。
3. 下载 callback 可复用现有 `Cloud189Client::download_app`，但仅在已获内部授权和可信同账户绑定后用于此次隔离 scope。独立 OAuth 自有目录客户端不是 QR/session 的替代认证，缺少 binding/schema/真实 HTTPS 核验时不得开启真实转存。
4. 若调用位置只有Cloud189Client而没有现有account state，最小可提供**crate-private**的shared-client/session-key/session-secret/cookie-login-user只读accessor或单个`transfer_parts()`（返回共享Client和必要凭据，不Debug/不日志）；SSON当前不在Cloud189Client中，需要caller现有账号state。本文实现不需要把request/send/json/app_headers开放，因为共用pure signing helpers与parse_json_safe已可复用；特别不要照搬目前固定GET的app_headers来签POST删除。
5. 公共路由只用 Open/AppShared 分享方法，禁止 AppOwned，也不自动转存；不能凭 shareId/fileId/query 参数授权内部 owned 操作。未来受控 kernel 调用须明确 shareId/fileId 顺序，保留当前 fast down 不转存的边界。

## 离线验证

```text
cargo test --test cloud189_transfer
JXPAN_LIVE_ORACLE=1 cargo test --test cloud189_transfer
node tools/cloud189-transfer-vectors.mjs --check
```

`JXPAN_LIVE_ORACLE`仅是项目已有的“重新运行原代码oracle”命名，**仍是mock fetch，不联网、不写入、不删除真实网盘**。原源码15个VM向量覆盖成功、字符串状态/错误、原下载失败泄漏、原删除失败仍返回URL和15次poll上限。Rust12项测试覆盖gates、strict状态、saved ID映射、所有叶文件、只删除本次文件、下载失败清理、失败manual、部分进度、异常树拒绝和原VM fixture。

原VM的storage全是内存/抛异常的mock，timer立即执行并记录delay。MD5用Node crypto垫WebCrypto缺失算法；HMAC仍走Node WebCrypto。所有凭据和下载URL是人工合成，fixture无真实秘密。没有真人授权根目录下的真实协议演练，没有账户写入，没有对createFolder/move作未经验证的“兼容完成”承诺。
