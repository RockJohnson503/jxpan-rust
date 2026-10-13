# 天翼云盘：当前部署协议与离线兼容基础

## 1. 基线、范围与证据

唯一研究基线为本次导出/解码的当前部署，不使用历史副本：

- `.scratch/baseline/_worker.js` SHA-256：`47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118`，对应 `manifest.json:18`。
- `.scratch/baseline/Cloud189Parser.js`（完整 1777 行）SHA-256：`29689a2973e07b11e27280ef3eaec0a4653d73267e81f9d15faf2bedccff46b2`。
- `.scratch/baseline/readable.js`：包含同一完整类、按需续期函数、目录树与播放快路径。
- `.scratch/baseline/handleMainRequest.js`：当前路由与认证状态机。

本文是**部署客户端行为**，不是官方服务端协议保证。源码中写“成功/已证明无效”的日志不是本次联网验证结果。未读取账号秘密、未登录、未保存/转存/删除，未联网。

本阶段只新增纯函数与离线差分测试，**未接入服务路由、未实现 HTTP provider**。

## 2. 已实现 API 与依赖

`src/providers/cloud189.rs`：

| Rust API | 与部署对应的语义 |
|---|---|
| `extract_share_info(&str) -> Option<ShareInfo>` | `extractShareInfo`，返回 shareCode、前缀/解码后的 shareUrl、原 RegExp 字符串或 `url_param_code` |
| `md5(&str) -> String` | UTF-8 MD5，小写，固定 32 位 |
| `java_md5(&str) -> String` | MD5 转正 BigInt 后输出十六进制，**去掉前导零**，不是固定 32 位 |
| `hmac_sha1(key, message) -> Result<String, SignatureError>` | UTF-8 HMAC-SHA1，小写 40 位；空 key 与原 WebCrypto 一样拒绝 |
| `open_download_signing_text` | Open API 下载原文，交给 `java_md5` |
| `open_list_signing_text` | Open API 列目录原文，交给 `java_md5` |
| `app_signing_text` | App 方法/路径/Date 原文，交给 `hmac_sha1` |
| `web_signing_text` | Web portal appkey + timestamp + sign-type，交给固定宽度 `md5` |

必要依赖（由主 agent 添加，不在本任务内修改 Cargo）：`md-5 = 0.10`、`sha1 = 0.10`、`hmac = 0.12`、`regex = 1`、`url = 2`。测试需要 `serde_json = 1`（可放 dev-dependencies）；本模块不需要直接 `serde` 依赖。没有 HTTP、时钟、随机数、存储或日志依赖。调用者显式传入时间/Date，避免偷偷改签名。

`tests/cloud189_vectors.rs` 使用 `#[path]` 引入独立模块，不要求修改 `src/providers/mod.rs`。主 agent 决定后续导出方式。

## 3. 链接识别完整行为

证据：`Cloud189Parser.js:53-88`。

1. 仅当输入以**小写** `http://` 或 `https://` 开头时保留，否则添加 `https://`；不 trim。
2. 对**整个字符串**执行一次 `decodeURIComponent`，不是仅解析 query。任何坏 `%` 或无效 UTF-8 都使整次解码失败，原字符串保留。`+` 不替换为空格。
3. 按下表顺序做不锚定、忽略大小写的正则匹配，命中立即返回。
4. 所有正则失败才 `new URL(url)`；取**第一个、大小写敏感的** `code` query 参数；JS UTF-16 长度 > 5 即接受，**不检查域名或分享码字符集**。URLSearchParams 在此第二阶段会再次百分号解码，且 `+` 变为空格。
5. 失败返回 null。

| 顺序 | 部署正则模式 | 字符/优先级后果 |
|---|---|---|
| 1 | `cloud\.189\.cn\/web\/share\?.*code=([a-zA-Z0-9]+)` `/i` | `.*` 贪婪：重复 code 往往取最后一个；也匹配 `xcode=`、fragment 内 code；JS `.` 不跨四类换行 |
| 2 | `cloud\.189\.cn\/t\/([a-zA-Z0-9]+)` `/i` | 下划线/连字符截断；h5/m 子域通常先被这一条命中 |
| 3 | `h5\.cloud\.189\.cn\/t\/([a-zA-Z0-9]+)` `/i` | 保留原规则，实际被第二条覆盖 |
| 4 | `m\.cloud\.189\.cn\/t\/([a-zA-Z0-9]+)` `/i` | 同上 |
| 5 | `cloud\.189\.cn\/s\/([a-zA-Z0-9_-]+)` `/i` | 允许 `_`、`-` |
| fallback | URL query `code` | 任意 host，任意字符；长度是 UTF-16 code units，例如三个 emoji 满足 > 5 |

这是**兼容提取，不是安全域名校验**。域名前缀、分享文本、其他网站的 redirect 参数都可能命中；路由本身的 `cloud\.189\.cn` 判断也未锚定。未来 HTTP 实现必须另设目标白名单/重定向规则，不能把此函数当授权边界。Rust `&str` 无法表示 JS 孤立 UTF-16 surrogate；`url` crate 与 WHATWG URL 的所有边角未穷尽证明相同。

## 4. 三种签名不能混用

证据：`Cloud189Parser.js:16-36,196,260-264,294,337,396,554,1414,1455`。

### Open API / javaMd5

下载签名原文（固定顺序，原值直接拼接）：

```text
AccessToken={openAccessToken}&Timestamp={milliseconds}&dt=1&fileId={fileId}&shareId={shareId}
```

目录签名原文：

```text
AccessToken={openAccessToken}&Timestamp={milliseconds}&fileId={folderId}&shareId={shareId}
```

目录的 uuid、shareMode、isFolder、iconOption、pageSize/pageNum、accessCode **不签入**。下载发送 `accesstoken/sign-type=1/signature/timestamp` headers；目录将相同字段放入 query。下载 URL 的 fileId/shareId 也直接拼接，目录 query 则使用 URLSearchParams；不能因“规范化”而改变签名原文。

### App / HMAC-SHA1

```text
SessionKey={sessionKey}&Operate={GET或POST}&RequestURI={path}&Date={UTC HTTP Date}
```

key 是 sessionSecret。path 不含域名、query 或 body；例如 `/getFileDownloadUrl.action`、`/open/share/listShareDir.action`、`/batchDeleteFile.action`。时间为 `toUTCString().replace(/GMT\s*/, "GMT")`；并非 `clientSuffix()` 的 `+0000` 格式。query/body 参数不在 HMAC 中。

headers 为 `Date/SessionKey/Signature`、`user-agent: desktop`、随机 `X-Request-ID`。query 后缀通常 `clientType=TELEPC&version=6.2&channelId=web_cloud.189.cn&rand={1..99999}_{1..9999999999}`。

### Web portal / 固定宽度 MD5

```text
600100422{milliseconds}1
```

headers `appkey=600100422/sign-type=1/signature/timestamp`，另带 `COOKIE_LOGIN_USER`，可加 `SSON`。原文没有账号 Cookie 或请求参数。Web 使用 `md5`，不是去前导零的 `javaMd5`。

## 5. Cloud189Parser 全方法/协议矩阵

下表覆盖完整类，不代表这些网络功能已在 Rust 实现。默认 GET，除非另写 POST。`A=https://api.cloud.189.cn`，`W=https://cloud.189.cn/api`，`E=https://open.e.189.cn`。

| 方法 / 源行 | 请求或本地行为 | 成功/失败与真实分支 |
|---|---|---|
| constructor:2 | APP_ID=8025431004，读取 flat config 的 env/accessToken/sessionKey/sessionSecret/openAccessToken/cookieLoginUser/sson | 不自动从嵌套 `cloud189` 加载 |
| md5/javaMd5/hmacSha1:16 | 第 4 节 | digest/importKey/sign 异常由调用方捕获 |
| generateUUID:37 | Math.random 的 v4 样式 UUID | 非密码学随机 |
| clientSuffix:43 | clientType=10020，version=7.1.8.0，channelId、毫秒 rand、Date +0000 | 下载 App 方法未使用该 Date 格式 |
| extractShareInfo:53 | 第 3 节 | 无网络 |
| getShareInfo:89 | A `/open/share/getShareInfoByCodeV2.action?shareCode=...&noCache=random`；可带 sessionKey | 顶层 `res_code===0 && fileId`，或不检查 res_code 的 `fileInfoAO.fileId`；规范 fileId/name/size/type/isFolder/shareId，**不保留 shareMode**；捕获 COOKIE_LOGIN_USER；失败 null |
| checkAccessCode:140 | POST A `/open/share/checkAccessCode.action`；form shareCode/accessCode/validateCode=；可带 sessionKey | 仅数字 `res_code===0 && shareId` 成功；保留响应 cookies 为 `_shareSessionCookies`；失败 `{success:false}` |
| getDownloadUrlByOpenAPI:190 | A `/open/file/getFileDownloadUrl.action?fileId=...&dt=1&shareId=...`；javaMd5 headers | 无 open token 立即 null；InvalidSignature 调 invalidateToken 后 null；InvalidAccessToken 按需续期，**最多一次**重试；其他 errorCode/null 失败 |
| getDownloadUrlByAppSign:252 | A `/getFileDownloadUrl.action`，HMAC headers | 缺 sessionKey/secret null；有 shareId 先 dt=1+shareId；失败再 dt=3+flag=1（不含 shareId）；成功看 fileDownloadUrl，不强制 res_code；异常直接 null，可能跳过第二种 |
| 下载链接规范:237/283/315 | `.replace("&amp;", "&")` 只替换第一次；不以小写 `http` 开头时前缀 `https:` | 不是健壮 URL 校验 |
| getWebSessionKey:331 | W `/portal/v2/getUserBriefInfo.action?noCache=random`，Web MD5/Cookie | cookie 缺失空字符串；读取 data.sessionKey；失败空字符串 |
| getRootDirectory:360 | W `/portal/v2/getRootDirectory.action`，Web MD5/Cookie；可带 web sessionkey | 数字 res_code===0 && fileId 成功，否则 null；saveAndDownload 实际不调用它 |
| saveShareFile:394 | POST W `/open/batch/createBatchTask.action`；form type=SHARE_SAVE，taskInfos=[{fileId,fileName,isFolder:0}]、targetFolderId、shareId；Web 签名 | 数字 res_code===0 && taskId；否则 null；**转存写操作** |
| checkBatchTask:437 | POST W `/open/batch/checkBatchTask.action`；form taskId/type=SHARE_SAVE | 数字 res_code===0；taskStatus===3 且无 errorCode 才成功；其他状态“任务进行中”或错误 |
| saveAndDownload:490 | 必须有 cookie；获取 webSessionKey → **固定根目录 -11** → saveShareFile → 最多 15 次，每次先等 2 秒 → getDownloadUrlByAppSign(fileId) → 成功后 deleteFile(fileId) | 仍使用传入 fileId，没从任务结果映射新 ID；下载失败/超时不清理；删除失败也返回 URL；不是 finally 清理 |
| deleteFile:548 | POST A `/batchDeleteFile.action`，HMAC，form fileIdList | 缺 sessionKey/secret false；数字 res_code===0 true；**删除写操作** |
| invalidateToken:586 | **当前部署只日志+return** | 保留已保存凭据；不删除、不持久化、不刷新 |
| ensureCookieLoginUser:590 | 已有 cookie 不请求；否则 cloud `/web/main/file/folder/-11` 带可选 sessionKey；再 cloud `/t/` | 捕获 Cookie；第二方式不带 sessionKey；异常被吞掉；默认 fetch 重定向，不是逐跳 cookie jar |
| parse:639 | ensureCookie → 提取 → 分享信息 → 有非空 trimmed pwd 时校验，但传原 pwd → folder 或零 size 时目录树；否则 token 循环 | 详细状态机见下节；甚至无效链接也先 ensureCookie |
| _extractCookies:857 | 优先 getSetCookie，过滤删除 cookie，取首分号前 name=value；无结果再 get(set-cookie) 按逗号拆 | 非完整 RFC cookie jar；Expires 内逗号存在风险；检查/部分取值使用 split('=')[1]，可能截断包含等号的值 |
| _isDeletionCookie:886 | max-age=0/-1 子串或 expires<=Date.now | 忽略删除项，**不从现有 map 移除旧值** |
| _mergeCookies:900 | 按 name 后值覆盖；existing 按 `; ` 拆 | 不保留 Domain/Path/Secure/Expiry 元信息 |
| initQRLogin:921 | 第 7 节 | 当前逐跳登录初始化与分域 cookie 行为 |
| checkQRStatus:1113 | POST E `/api/logbox/oauth2/qrcodeLoginState.do` | 返回原 JSON；异常 status=-999；**没有发送保存的 Cookie，也没有提取/设置 _pollCookies**；deviceId 未使用 |
| exchangeSession:1156 | POST A `/getSessionForPC.action`，query suffix TELEPC/version=7.1.8.0/channelId/rand/redirectURL；无 form body | 数字 res_code===0 返回 sessionKey/secret/accessToken/refreshToken，捕获 Cookie；失败 success:false |
| getCookieLoginUser:1213 | cloud `/api/portal/loginUrl.action` manual → 最多 10 个 Location；失败再 redirectUrl 最多 10 次 | 第一方式 Location 不做相对 URL 解析、跨域携带累计 cookies；第二方式仅 `/` 相对路径拼 origin；不是真实分域 jar；只返回 Cookie/SSON |
| _filterCookiesByDomain:1332 | 仅保留含 `=` 项 | **domain 参数完全未用**，不是域名过滤 |
| getOpenAccessToken:1347 | W `/open/oauth2/getAccessTokenBySsKey.action?noCache=...&sessionKey=...`；appkey/Referer/可选 cookies | 有 data.accessToken 即成功，不检查 res_code；失败返回 error |
| listShareDir:1383 | V1 → V3 → 有 Open token 时 Open API → 有 session pair 时 App | `[]` 是成功，只对 null 降级 |
| listShareDirOpenAPI:1412 | A `/open/share/listShareDir.action`，第 4 节签名 query，pageSize=200/pageNum=1 | uuid/shareId/fileId/shareMode/isFolder=true/iconOption=5/accessCode；可带分享 cookies；没有 InvalidAccessToken 自动续期分支 |
| listShareDirAppSign:1453 | 同 A path，HMAC headers + 通用列表 query/App suffix | 可带分享 cookies；无分页循环 |
| listShareDirV3:1499 | 同 A path，**无签名**，同列表参数 | cookies 优先 _shareSessionCookies，其次 COOKIE_LOGIN_USER；**当前不再 sessionKey 重试**；无论 res_code，交 parseFileListFromResponse |
| listShareDirV1:1544 | W `/open/share/listShareDir.action`；模式依次 [shareMode, shareMode==='1'?'2':'1', '3'] | pageNum=1/pageSize=200、fileId/shareDirFileId、isFolder、shareId/mode/iconOption=5/orderBy=lastOpTime/descending=true/accessCode/noCache；sign-type=1 **没有 signature**；可带 sessionKey；Cookie 优先 COOKIE_LOGIN_USER 再分享 cookies；res_code 数字/字符串 0 才解析 |
| parseFileListFromResponse:1604 | 只接受数字/字符串 res_code 0；先 fileListAO.folderList/fileList，不产项才 top-level 列表 | 文件夹先于文件；id/name 双字段 fallback；size 使用 nullish 而不是 truthy；folderList 强制 folder；fileList.isFolder 只认数字 1 或 bool true，不认字符串 '1'；空列表返回 [] |
| browseFolderInteractive:1660 | **当前全树** jxpanWalkDirTree concurrency=6/maxNodes=400；根 shareInfo.fileId；缓存 c189tree:shareId|password 600000ms | 输出 `{code:200,shareKey:'c189:'+shareId,data:{tree,sid}}`，无旧版 idMap/_shareInfo/_password；找不到节点 code400；截断/失败节点不等于完整分页结果 |
| resolveItemInFolder:1695 | 仍读取旧 `_shareInfo/idMap/_folderTreeCache` 格式 | 新全树不含 idMap，通常先 404；文件 token 循环没设置 openAccessToken；把 currentToken 当 getDownloadUrlByOpenAPI 第三参 retried，非 token；saveAndDownload 的 shareId/fileId 顺序反了；失败仍 code200/success=false |

## 6. parse 与下载快路径的实际差异（当前补丁重点）

### 常规 parse

证据：`Cloud189Parser.js:639-856`、`handleMainRequest.js:9009-9164`。

- 常规路由 token 依次：前端 `c189_token.trim()` priority1 → D1 `c189_login_default` 有效 Open token priority2（长度>20，has_valid_open_token !== false，expires_at>now）或 App-only priority2.5（必须有 app token/session pair）→ `P189_TOKEN/p189_token/CLOUD189_TOKEN/cloud189_token/CONFIG.cloud189.accessToken` priority3 → D1 `cloud189_default_config` 的 open_access_token/accessToken priority4。后两来源只带 token，**未设置 has_valid_open_token**；KV 分支当前都是 null。
- 列表按插入顺序尝试，不另排序；部分来源去重。未找到 token 时路由直接 JSON code401。
- 类的 isAppTokenOnly = source 包含 `_app` 或等于 `app_only` 或 `!has_valid_open_token`。因此 env/storage/config token 缺标志时也跳过 Open API。不可按注释假定“所有 token 都会先 Open”。
- 单文件每 token 实际 **A Open → C 转存 → B App**；App-only 跳 A。C 仅 cookie+shareId；B 仅 session pair；不是日志暗示的 A→B→C。
- folder 或 `!fileSize`（包括零字节文件）须 shareId；无 Open token 但有 sessionKey 会先取 Open token，然后全树返回；此时**不进行单文件 token 降级循环**。shareMode 已在 getShareInfo 规范化时丢失，通常默认 '1'。
- 校验失败 code403；无信息/无 fileId/目录无 shareId code400；无 token code401；全认证失败 code500，带 session pair 时 need_relogin；正常文件返回 snake_case download_url/files；异常 code500。大部分是 JSON 内的 code，HTTP 状态默认 200。

### type=down 快路径：不能让常规 parse 代替

证据：`handleMainRequest.js:14-73`、`readable.js:14697-14806`。

- 入口 `type=down && cleanFileIdParam && targetUrl`，**在 action 处理和常规 provider 解析之前**。`id` 优先 `d`；`?webpage=` 从 ID 分离；sid 可直接作 shareId。
- 进程内播放缓存 key `play:{targetUrl}|{fileId}|{pwd}`，TTL 10 分钟、最多 256 条，命中直接 302；key **不含 sid/账号/认证信息**。
- 快 parser 构造 `{...CONFIG,env}`，读取 D1 session pair/Cookie/SSON；Open token 长度>20 即设置，**不检查 expires_at 或 has_valid_open_token**；并不装载常规 c189_token/env alias/allTokens 降级队列。CONFIG 的嵌套 cloud189 token 不会由 constructor 自动展开。
- **已有 sid 时不提取分享、不校验密码、不调用 getShareInfo**。无 sid 才获取分享信息，有 pwd 才验证；验证失败不会单独 403，只在缺 shareId 时失败。若分享根等于 fileId 且 isFolder 或零 size，拒绝目录。
- 缓存目录树发现 fileId 是 node 也拒绝；快路径 **先 App 签名，再 Open API**，不转存、不扫目录、不调用 ensureCookieLoginUser。
- 拿到 URL 后 **GET + Range: bytes=0-0 + redirect:manual** 试探一跳，仅 3xx+Location 时替换；不 resolve 相对 Location、不遍历后续跳转，探测异常仍保留原 URL。
- 最终含 `download.cloud.189.cn/file/downloadFile.action` 按目录拒绝，不尝试代理/常规 fallback。目录为 HTTP400/JSON code400“目录无法下载”；失败 HTTP502/JSON code502；成功 HTTP302/Location，CORS `*`、Cache-Control:no-cache、Content-Type:video/mp4。
- Cloud189 快路径失败直接返回，**明确 no fallback**。成功 302 写入播放缓存。

### 常规输出跳转

`readable.js:11894-11966`：type=down 强制 shouldRedirect，type=json 禁用，否则使用配置；只有 code200 + data.download_url 且非 folder/directory/list 才跳。Cloud189 直接 HTTP302 到 URL，不代理；并不做快路径 Range 探测。认证/文件 action 通常直接返回 JSON，不走此 handleResponse。

### 当前目录 action 的格式不一致

`handleMainRequest.js:1437-1581` 的 parse_cloud189_folder_item：必须 share_id+target_file_id；D1 或 CLOUD189_TOKEN 装载；可重验 pwd（失败只日志），可获取 Open token。无 folder_id 直接 browse；有 folder_id 则 browse 再 resolveItemInFolder，后者还依赖旧 idMap，和当前 `{tree,sid}` 输出不匹配。

`handleMainRequest.js:1614-1766` 的 parse_cloud189_file：前端 c189_token → D1 → CLOUD189_TOKEN；ensureCookie/可重验 pwd/可取 Open token，然后 **Open → App → 转存**，不同于 parse。其转存调用 `saveAndDownload(fileId,shareId,fileName)` 参数倒置。可追加 filename，输出 camelCase `downloadUrl` 的 JSON。这些遗留路径不能作为正确 Rust 协议设计照抄，必须单独覆盖。

## 7. 认证路由完整状态机

### 二维码初始化：真实逐跳 Cookie

`Cloud189Parser.js:921-1112`、`handleMainRequest.js:3705-3790`：

1. cloud `/api/portal/unifyLoginForPC.action?appId=8025431004&clientType=10020&returnURL=https://m.cloud.189.cn/zhuanti/2020/loginErrorPc/index.html&timeStamp=...`，**redirect:manual**，最多 5 次 redirect；每跳提取 Cookie，维护累计、cloud 域、openE 域三份字符串；按当前 host 选择发送 cloud/openE cookies。Location 使用 `new URL(location,currentUrl)`。
2. 最终 HTML 正则取 lt（16+ hex）、reqId（10+ 字符）、paramId。缺 lt 则 E `/api/logbox/separate/web/index.html?appId=8025431004` manual；有 Location 再请求一次，从跳转 URL query 取 lt/reqId；不是完整循环。
3. 缺 reqId 生成 32 个随机 hex；userFinger 是随机 10 位十进制；POST E `/api/logbox/oauth2/getUUID.do` body appId。headers Origin、登录页 Referer、user-finger、可选 lt/reqid，**只带 openE 域 cookies**；合并响应 Cookie。
4. qrUrl 是返回 uuid 的完整值，encryuuid 独立保存；路由保存完整 qrData 到 D1 `c189_qr_{deviceId}` TTL600 秒，并同时存内存 exp=10分钟。返回 expires_in=300、interval=3、has_storage；声明二维码期限不等于存储期限。

### 轮询、交换、保存

`Cloud189Parser.js:1113-1382`、`handleMainRequest.js:3792-4033`：

- cloud189_check 的 device_id 只取 URL query。优先 D1 对象/字符串，然后内存（校验 exp）；KV 读取当前 null；无 QR code400。
- checkQRStatus form：appId/clientType=10020/returnUrl/paramId/uuid（完整 qrUrl）/encryuuid/date（UTC ISO 去 T，秒精度）/timeStamp；headers lt/reqid/Origin/Referer/UA。**原类未带 qrData cookies，未返回 _pollCookies**。路由虽合并 result._pollCookies，当前通常为空；cloud189OnlyCookies 计算后不用于 exchange。
- `status===0 || redirectUrl` 即进入交换（有 redirectUrl 即使其他 status 也会进入）。exchangeSession POST getSessionForPC，成功后若无 COOKIE_LOGIN_USER，访问 getCookieLoginUser 两个 manual redirect 方案。
- 用 exchange 捕获 Cookie/SSON + 保存的 openE/累计 cookies 组字符串，向 getAccessTokenBySsKey 申请 Open token；可能重复 cookie name，也不是严格域过滤。失败保留 App-only，不阻止登录成功。
- 保存 session_key/session_secret/app_access_token/open_access_token/access_token（Open 优先）/refresh_token/cookie_login_user/sson/login_time/expires_at/has_valid_open_token/token_source。**当前本地 expires_at = now+3650天，D1 expirationTtl=0**，不是上游 token 有效期证明。
- D1 先 delete 再 put，非事务；存储异常仅日志，仍返回登录成功及**完整 loginInfo**；clearAllCookieCache 仅保存成功调用。Rust 基础模块不复制这些秘密日志/响应行为。

| cloud189_check 上游分支 | 返回 JSON |
|---|---|
| status===0 或 redirectUrl | exchange；成功 code200/status=success/loginInfo，交换失败 code500 |
| status=-106 或 -1 | code200/status=waiting |
| status=-11002 | code200/status=scanned |
| status=-11001 | code400/status=expired |
| result=-20099 或 msg 包含“过期” | code400/status=expired/error_code |
| 其他，包括 checkQRStatus 的异常 -999 | code200/status=polling/raw_status |
| 路由异常 | code500 |

### 状态、手动保存、退出、按需刷新

| 路由/函数 / 证据 | 当前行为 |
|---|---|
| cloud189_status，handleMainRequest:4034 | 仅 D1；stored 直接 JSON.parse（不像解析路由支持对象）；无记录 logged_in=false；`!expires_at || now>expires_at` 才 expired，和常规解析的 `>=` 有边界差异；仅本地状态，不验证上游；不 fallback env |
| cloud189_manual_save，:4151 | access_token 只取 query；不空即可同时标为 app/open token，session pair 空；30天 expires/TTL；不设置 has_valid_open_token；无 D1 也返回成功；未校验 token 真伪 |
| cloud189_logout，:4123 | 只删 D1 c189_login_default；没有清播放缓存、QR内存或环境 token；并非全凭据退出 |
| jxpanCloud189RefreshOnDemand，readable:10962 | 只有 env.jxpan + 已保存 app_access_token 才续期；GET A getSessionForPC.action，query TELEPC/version6.2/channelId/rand/appId/accessToken；要求 HTTP200+数字 res_code0+session pair；凭已有 Cookie/SSON取 Open token；两步都成功才更新 session/open/access_token/has_valid_open_token/login_time/expires_at=3650天/last_on_demand_refresh_at/token_source，并 storePutRaw 无TTL选项 |
| InvalidAccessToken，Cloud189Parser:219 | 仅 Open **下载**方法调用上述刷新，更新当前 parser 后一次重试；listing/扫码/status 不触发；不依据本地 expiry 预先刷新 |
| InvalidSignature / invalidateToken，:214/:586 | 当前不删 token，不清保存状态；不是自动重新登录 |

认证 action 位于 `handleMainRequest` 的一般 action 分支，所读范围未发现这些分支自身的管理员鉴权门槛；不能因为 `/admin` 有单独处理就假定 action 受其保护。需主 agent 另查整个部署入口的外层控制，不在本基础模块中修改。

## 8. 差分测试与完成边界

`tools/cloud189-vectors.mjs`：

- 校验当前 raw worker SHA，确认提取的完整类原文包含于 readable；在 Node VM 加载**同一个当前部署类**，不是重新写一个 JS 算法。
- 49 个固定链接输入；135 个固定 MD5/javaMd5 输入（含前导零、Unicode/NUL）；5 个固定 HMAC 输入（含空 key 拒绝、多字节/长 key）；固定时间 `1704164645000`、固定随机。
- HMAC 原方法调用 Node WebCrypto；MD5 的 SubtleCrypto 测试 shim 仅委托 Node createHash('md5')，保留原方法的 UTF-8 与十六进制/BigInt转换。
- 用**原下载/目录/App/Web 根目录方法**捕获 6 个真实签名调用原文、输出、请求字段；fetch 是严格路径白名单的纯内存响应。不会调用 initQRLogin、exchangeSession、saveShareFile、saveAndDownload、deleteFile、存储函数或真实网络。
- 所有 token/session/Cookie/password 均是工具内 `offline-*` 合成值；VM console 被禁用，不泄露原日志；没有读取秘密。

`tests/cloud189_vectors.rs` 调用上述工具并将实际 Node 输出与 Rust 提取、散列、签名原文/输出/线传字段逐项比较；缺 Node、缺 baseline、哈希变化都失败，**不 silent skip**。测试生成固定输入的预期值是原方法现场求值，不是手写期望。

```text
node tools/cloud189-vectors.mjs         # 输出仅合成的离线 JSON 向量
cargo test --test cloud189_vectors     # 主 agent 添加依赖后运行 Rust/Node 差分
```

本子任务已运行 Node oracle：SHA 正确、类 SHA 正确，49/135/5/6/6 数量正确。**Rust 对比测试文件已实现，cargo 编译/执行由主 agent 整合运行；在它实际通过前，不声称 Rust 差分完成。** 原文拼接已覆盖 GET 下载/列目录与 Web；删除 POST 的原方法禁止执行，因此没有执行 POST 删除格式差分。任意方法字符串原样拼接的纯函数不等于已实现删除。

未实现：网络客户端、账户/扫码/续期、Cookie jar、提取码校验、文件信息转换、目录列表/分页/全树/cache、下载降级、转存/删除、播放探测/302、参数校验与安全域白名单、HTTP路由/部署。未做上游联网验签或真实账号验证。源码中的 format mismatch/参数倒置/日志泄密均记录为集成风险，本任务不修改共享 JS 或 Rust 主路由。
