# 移动云盘：当前部署协议与离线兼容模块

## 0. 来源、边界与复现

只依据当前实例 `.scratch/baseline/`，不引用历史副本、抓取网页教程或猜测标准实现。

| 文件 | SHA-256 |
|---|---|
| `_worker.js`（部署源） | `47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118` |
| `readable.js`（展开部署源） | `7196db8435e214bc0bd46f60534a247652cf426185c272a9bd9bd0c3b15eaa79` |
| `MobileCloudParser.js`（原类） | `4c2784a32492b016548fcbf6286a71fd2ca79b995280aeb3eff8d45bba680661` |
| `handleMainRequest.js`（原路由） | `156122bbbc652bc133a92c28e8d870eb15ab52f4fb902a84d52af9a3500cfddf` |

`tools/mcloud-vectors.mjs` 在隔离 Node VM 中执行**未重写的原类同名纯方法**，并执行部署的 `md5` / `pythonQuoteExact` / `generateMcloudSign`。不加载 worker 入口、路由、登录、续期、目录、下载方法；VM 无 fetch、存储、定时器、process、require、crypto.subtle。固定 VM 内时间和随机字节，仅为了让签名确定性复现。Node VM 是此处可信部署源码的执行隔离措施，不宣称它是运行任意不可信代码的安全沙箱。

```sh
node tools/mcloud-vectors.mjs --summary
# 整合者添加依赖后运行，测试自动执行上述 VM，不使用手写期望值：
cargo test --test mcloud_vectors -- --nocapture
```

新增模块仅做离线函数：分享链接识别、Authorization 字节编解码/账号提取/请求头清理、原类 isFolder、当前部署签名内核。没有网络、日志、凭据读写、登录、续期、转存或删除。

依赖要求：`base64 = "0.22"`、`serde_json = "1"`。不要求 url、regex、serde derive、md-5、AES 或 RSA。签名接收 **原样 JSON.stringify 字符串**，因此不要求 serde_json 的 preserve_order；调用者绝不能把有序 JS JSON 重新序列化成另一个顺序后声称同等签名输入。

## 1. 字段词典与入口

| 字段 | 当前实例含义 / 类型 / 特别规则 |
|---|---|
| `url` | 分享输入，POST 表单优先于查询参数；普通解析路由要求 http(s)，原类纯识别器更宽松 |
| `pwd` / API `passwd` | 提取码，默认空字符串；不从分享文本自动抽取密码 |
| `type=down` | 文件直链播放捷径；只有同时具备目标 URL 和 id/d 才进入 |
| `id` / `d` | 文件 ID，id 优先；文件 ID 中 `?webpage=` 后缀会剥离 |
| `fid`, `fid1`…`fid20` | 目录链，按序收集，后续重复项去重；普通类 parse 只使用最后目录 ID |
| `sid`（解析响应/下载请求） | 移动云**分享 linkId**，不是登录会话，也不是文件 ID；down 时优先于 URL 提取 |
| `sid`（二维码） | 独立随机 16 字符登录会话标识；不得与分享 sid 混用 |
| `linkId` / API `linkID` | 分享 ID；类识别器仅 ASCII 字母数字；结尾小写 `https` 被无条件删除 |
| `pCaID` | 目录 ID，根固定字符串 `root` |
| `coID`, `contentID`, `id`, `fileId` | 原类文件/目录 ID 候选，按此顺序做 JS `||` 取首个 truthy 值 |
| `path`, `coPath` | 原类单文件下载所用路径候选；与文件 ID 不保证等价 |
| `coName`, `contentName`, `name`, `fileName` | 原类显示名候选，按此顺序 |
| `coSize`, `contentSize`, `size`, `fileSize` | 原类大小候选，最终经 formatFileSize；0 会被 `||` 跳过 |
| `authorization` / `auth` | 字符串凭据或凭据对象字段；对象取 authorization || auth；请求头生成只支持字符串 |
| 解码 `prefix:account:token` | 扫码生成 prefix 固定 `pc`；续期解析 token 为第三字段起的所有冒号段重新连接 |
| token `|` 第四字段 | 续期判断的毫秒绝对到期值（Number 转换）；不是秒，也不是本地存储 timestamp |

入口顺序见 `handleMainRequest.js:8-73`。fast-down 位于常规 action、前端凭据解析及普通缓存读取**之前**，不要假定它已应用后面的 auth 参数逻辑。

## 2. 凭据来源与准备

1. `getConfig` 初始配置：enabled 默认为真，authorization 从 `MCLOUD_AUTHORIZATION` 或默认配置取得；cookie 和 userAgent 也有配置字段，但原 MobileCloudParser 的分享请求不使用 cookie，UA 是硬编码。见 `readable.js:2008-2013,2053-2058`。
2. 统一凭据源另外使用 `MCLOUD_AUTH` / `mcloud_auth`，与上面的环境变量名称并不一致。有 D1 绑定时顺序 D1 > 前端 > env；无 D1 且有效前端时前端 > env > D1。前端字符串需长度 >50 或对象含 token/cookie/authorization。D1 键为 `mcloud_login_default`。见 `readable.js:14500-14512,14634-14696`。
3. 普通解析另有直接读取 D1 并提取 authorization || auth 的路径，优先扫码存储；然后准备函数又可能覆盖 CONFIG。不能将所有分支概括成一个全局不变优先级。见 `readable.js:23333,23495-23496,24015-24016`。
4. `jxpanPrepareMcloudCredential` 必须 env.jxpan 存在。stored 是对象时浅复制；是字符串时直接当 authorization，并非先 JSON.parse。无字符串 authorization/auth 时返回 false。
5. 准备函数删除开头 `/^basic\s+/i` 后 atob，但**不先 trim**；解码失败保留原凭据。按冒号拆成 prefix、account（可回退 info.phone）、token（保留后续冒号）；缺 account/token 时保留原凭据。
6. token 的第四个 `|` 字段经 Number 转为毫秒 expiry；有限、正数且 <= now +15 天才尝试续期。上次失败距今 <1小时不重试。
7. POST `https://aas.caiyun.feixin.10086.cn/tellin/authTokenRefresh.do`，Content-Type XML，body `<root><token>…</token><account>…</account><clienttype>656</clienttype></root>`；XML 转义 & < > 双引号 单引号。HTTP ok + `<return>0</return>` + 非空 `<token>` 才成功。
8. 成功生成 `Basic ` + btoa(prefix:account:newToken)，更新 phone、mcloud_auto_refreshed_at，移除 failed_at；失败记录 mcloud_auto_refresh_failed_at 并继续用旧凭据。标记 jxpan_auto_refresh_managed、timestamp，encryptToKV 或明文后 storePutRaw（无显式 TTL）。返回值仅表示 Authorization 是否改变，**不是凭据有效性认证**。见 `readable.js:11011-11089`。

原类账号提取独立且更宽松：trim → 删除大小写不敏感 Basic+空白 → atob → 取第一个 pipe 段 → 冒号下标 1（否则整段）；空账号可返回空字符串。输入 null/错误类型/无字段为 null，非法 base64 为字面 `default`。atob 返回 Latin-1 字节字符串，不能直接当 UTF-8。原类会打印完整原始/解码凭据，Rust **不复制这些敏感日志**。

`buildHeaders` 则先移除所有 JS `\s`，只有大小写准确的 `Basic` 才重新插入一个空格；`basic` / `BASIC` 不规范化。凭据对象虽可用于账号提取，调用 `.replace` 时却会抛错。

## 3. 二维码登录与保存（仅研究，不执行）

见 `readable.js:21669-21963`：

| 阶段 | 请求/关键行为 |
|---|---|
| mcloud_qrcode | clientId `10701`、version `7.17.2`；UUID 去横线产生 deviceId(32)、sid(16)、aesKey(16) |
| 公钥 | POST `https://yun.139.com/orchestration/auth-rebuild/key/v1.0/getRsaPublicKey`；JSON `{clientCode:"10701",type:"1"}`；带 mcloud-sign |
| 密钥封装 | publicKey 为 DER base64，解析 ASN.1 SPKI / RSA modulus+exponent；RSA PKCS#1 v1.5 加密 aesKey，输出 base64 到 mcloud-skey；失败时留空且仍可能继续 |
| 暂存 | 固定全局键 `mcloud_session` 保存 deviceId/sid/aesKey/mcloudSkey，TTL 300秒；不是按用户或 sid 分键，并行登录会互相覆盖 |
| 二维码 | `https://yun.139.com/w/#/qrcLogin?sID={sid}&dID={deviceId}&cType=9`；expires_in 300、建议 interval 3秒 |
| mcloud_qr_poll | 必须存储存在且 session 未过期；AES-ECB 加密内层 JSON `{dycPwd:sid,loginStyle:"QRCode",clientEnv:"3",setCookie:0}` |
| 登录 API | POST `https://yun.139.com/orchestration/auth-rebuild/permission/v1.0/login`；外层 `{encryptMsg,clientId:"10701",returnToken:true}`，带 mcloud-sign、mcloud-skey |
| 成功 | loginData.success 且 data.result.resultCode==="0" 且 authToken || token；phone=atob(encryptAccount || "")，**只在 atob 抛错时**回退 simplifyAccount；空 encryptAccount 解码成功为 ""，不会回退 |
| waiting/scanned/expired | 200059541→waiting；200059548→scanned；200059542/200059549→expired；其它结果默认 waiting；缺 session 返回 400 expired |
| Authorization | `Basic ` + btoa(`pc:{phone}:{token}`)；响应 confirmed 返回 authorization、phone |
| mcloud_qr_save | 查询参数 authorization、phone；不校验 token/账号；保存 authorization/phone/timestamp 到 mcloud_login_default，加密失败回退明文；实际 expirationTtl=0，日志所谓“3天”与代码不符 |

RSA 实现用随机非零填充、BigInt 模幂，不属于本次 Rust 范围。AES-ECB 实现利用 AES-CBC 的零 IV 对每块分别加密再只取第一块，手动 PKCS#7 字节填充。见 `readable.js:3634-3723`。

## 4. AES 与签名：不要用标准库替换后宣称兼容

### 分享 API AES

`AESUtils2` 使用源码固定 16字符 key（此处不重复其值），AES-CBC，随机 16字节 IV；传输为 base64(IV || 密文)。它先按 **JS 字符串 UTF-16 length** 手动追加填充字符，再 UTF-8 编码，然后 WebCrypto **再次**自动 PKCS#7 填充。解密 WebCrypto 已去一层，又根据末字节裁掉一层。非 ASCII 手动填充长度不等于 UTF-8 字节长度；不能直接实现单层标准 CBC/PKCS#7 视为一致。失败时 encrypt 返回明文、decrypt 返回输入原样。见 `readable.js:10915-10960`。

### 当前 generateMcloudSign

见 `readable.js:3603-3633,3724-3830`：

1. timestamp 是 runtime 本地时间 `YYYY-MM-DD HH:mm:ss`，不强制 UTC/+08。
2. nonce 16字符：随机 byte %62 映射 `A-Z a-z 0-9`，存在模偏差。
3. JS-truthy body → JSON.stringify（不排序对象键）→ pythonQuoteExact：只保留字母数字，其它 UTF-8 字节 `%XX` 大写。该函数遍历 UTF-16 单元，emoji 的两个 surrogate 分别经 TextEncoder 变 U+FFFD，**不是通常的 encodeURIComponent**。
4. 对 quote 后字符按 charCode 排序 → UTF-8 → base64 → r=部署 md5。
5. c=部署 md5(timestamp + ":" + nonce)，sign=部署 md5(r+c).toUpperCase()；header=`timestamp,nonce,sign`。
6. deviceId/version 参数完全未参与签名。

**部署的 `md5` 不是标准 MD5！** 每一轮额外执行 `b=b+aa|0`，稀疏 words 缺项使该轮 f=NaN、位运算变0，最终按32位字大端 hex 输出。原函数 md5("")=`3cc0c603fb18b78e3cc0c6024cf31a79`，不是 RFC 1321 的值。本模块逐轮保留此行为，不能换成 md-5 crate。分享 API buildHeaders 并未发送 mcloud-sign；签名只用于登录相关 API，不必给分享请求增添它。

## 5. 分享验证、分页与目录

分享元数据/目录 POST：

`https://share-kd-njs.yun.139.com/yun-share/richlifeApp/devapp/IOutLink/getOutLinkInfoV6`

```json
{"getOutLinkInfoReq":{"account":"<account>","linkID":"<linkId>","passwd":"<pwd>","caSrt":0,"coSrt":0,"srtDr":1,"bNum":1,"pCaID":"root","eNum":200},"commonAccountInfo":{"account":"<account>","accountType":1}}
```

- 用字符串模板 replace 而非 JSON.stringify，特殊引号/反斜杠可能破坏请求；请求体 AESUtils2.encrypt139，Content-Type 仍 JSON。
- buildHeaders 固定 Caller web、Mcloud-Client 10702、Channel 1000101、Route 001、Version 7.13.3、Origin/Referer yun.139.com、固定 Edge UA/x-deviceinfo。与登录的10701/7.17.2不同。
- 每次请求 30秒 AbortController；仅 HTTP200 + 非空响应继续。AES 解密后 JSON 成功条件是 truthy result.success **或严格字符串** resultCode==="0"。numeric 0 不单独满足。
- 文件数组取 data.coLst || fileList || files || contentList || []，文件夹 data.caLst；空数组 truthy，会阻止后续 fallback。
- 文件夹归一化成 coID=caID||id，coName=caName||name||默认，coType=1、isDir=true、path=ca.path||"/"+ID。
- JSON 失败有截断救援：扫描 caLst 完整括号段、JSON.parse 或提取首个 caID/caName；coLst 正则/简易深度扫描。不是可靠 JSON 流式解析，也不证明密码验证成功。
- **当前类没有分页循环**：所有目录永远 bNum=1/eNum=200；多于200项可能丢失，返回 have_page=false/tree_complete 不意味着真的完整。没有独立校验提取码接口，passwd 是此目录调用的字段；各种鉴权、密码、空目录、HTTP和解密错误大多统一成 [] /404。

见 `MobileCloudParser.js:144-361`。

### 目录判断与两种遍历不一致

原类 isFolder 仅以下任一严格条件：coType===1 / resType===1 / isDir===true / isDir==="1" / type==="folder"。字符串 coType="1"、isDir=1、isFolder=true 不单独满足；null 访问字段抛错。

但共享 `jxpanWalkDirTree` 用 `!!(f.isFolder || f.isDir || f.coType===1)`，接受 isDir="0"、数字1，却不认 resType/type。字段 ID 顺序也变成 fileId || id || coID || contentID；名称 fileName || name || coName || contentName；大小 fileSize || size || coSize || contentSize。这些差异是现存行为，本次仅实现**原类**判断，不合并修正。

- parse 开关禁用→503；未配置 Authorization→401；无可用 linkId→400；账号 falsy 时回退 default。
- fidChain+fileId→parseFileById 从根搜索，不限于链；只有 fidChain→parseMcloudTree 使用最后一个ID；fileId无链也根搜索。
- 单文件且无目录时返回 list、download_url=null、shareKey=`mc:{linkId}`、current_folder_id=root、tree_complete=true；不预取下载。
- 多文件/有目录走整树：并发4、maxNodes400、seen 去重；超限/上游失败没有完整性标记。全树内存缓存键 `mctree:{linkId}|{pwd}`，10分钟，最多64条，与账号无关。
- nested tree 为 `{展示根名:{文件名:{id,size},目录名:{…}}}`，重名追加 `#{fid}`。walk 去重不等于 nested build 有防环，异常目录环仍有递归风险。
- 未找到 folderId 时 node 会回退 root，但 nestedTree 仍使用想要的 want ID，可出现空子树。
- parseFileById DFS 深度0..8、visited 去重，找到目录→400；找到文件→parseSingleFile。逐目录串行，无分页。
- 专门 action=parse_mcloud_subfolder 走 parseWithFidChain，逐个 fid 直接请求 pCaID，并不确认它真是上一级的孩子；用的是更宽松 [a-zA-Z0-9_-]+ 提取式或最后URL段，不是 extractShareInfo；构造器传 env 而非 CONFIG，可能缺 mcloud 配置。见 `handleMainRequest.js:1069-1128`。

见 `MobileCloudParser.js:459-802`、`readable.js:14831-15023`。

## 6. 文件路径、sid、下载302与缓存

下载 POST：`https://share-kd-njs.yun.139.com/yun-share/richlifeApp/devapp/IOutLink/dlFromOutLinkV3`

```json
{"dlFromOutLinkReqV3":{"account":"<account>","linkID":"<linkId>","coIDLst":{"item":["<filePathOrId>"]}},"commonAccountInfo":{"account":"<account>","accountType":1}}
```

- 同上 AES/headers/30秒，仅HTTP200；success || resultCode==="0"，URL取 redrUrl || downloadUrl || url；JSON失败则正则救援相应字段。失败→null。下载请求**没有 passwd**。
- 原类 parseSingleFile 使用 path || coPath，不用 coID 兜底；路径空仍返回解析成功但直链null。
- 当前 fast-down 却直接将文件 id/d 传 getDownloadUrl 的 filePath 参数。ID与路径双重用法未由源码证明对所有分享成立，应单独联网验证，不应在离线模块擅自拼路径。
- shareId/sid 优先，否则从URL提取 linkId；准备凭据后创建 parser。tree缓存里存在同ID目录时400；没有缓存则不能预判目录，直接请求下载。
- 成功直接HTTP302，Location=上游URL，CORS=*，Cache-Control=no-cache；失败HTTP502 JSON，移动云不回退常规解析。
- play缓存键 `play:{targetUrl}|{cleanFileId}|{pwd}`，10分钟；缓存命中发生在凭据准备前，键不含sid或账号。`jxpanPlayCacheSet`最多256条（见下述源码），不要与目录缓存64条混淆。
- 普通持久解析缓存键 `parse_`+base64(UTF8(url+"|||"+pwd+可选"_"+fidChain.join("_")))，不含fileId/账号。移动云会走常规缓存，但 fast-down 在其前；缺data.tree的移动云缓存被丢弃，后续有效性判定另要求download_url/list/tree_complete，可能把tree-only新结果再次视为无效。这是代码分支冲突，不能仅凭tree缓存日志说命中稳定。

见 `MobileCloudParser.js:362-458,563-581`、`handleMainRequest.js:36-73`、`readable.js:14697-14830,3831-3861,22045-22090`。

## 7. 完整行为矩阵、差分范围与风险

| 类别 | 当前行为 | 此次 Rust / 差分 |
|---|---|---|
| 链接 | 5个正则按优先级；单次URI解码失败保留原串；容许任意域fallback；仅字母数字；删除小写https尾 | extract_share_info，2909向量 |
| Auth账号 | JSON类型/null/对象字段truthiness；trim/Basic大小写；Latin-1 atob；空账号/default | extract_account_from_auth，225向量 |
| btoa/atob | 缺padding、非法padding、空白、非规范尾bits、Latin-1、拒绝Unicode btoa | encode 11 + decode 18 |
| header | 去JS空白；仅Basic准确大小写补空格；只支持字符串 | 9向量，仅Authorization字段 |
| Folder | 严格类型条件；null错误；不等于walk判断 | 1407向量；try_is_folder保留null错误 |
| 签名 | 非标准部署md5、UTF16逐单元quote、固定时间熵、原JSON | md5 168 + quote 7 + sign 14 |
| 登录/凭据源 | QR/RSA/ECB、D1/env/frontend、save | 仅静态梳理，不执行/不实现 |
| 续期 | 15天窗口、1小时失败退避、XML更新、存储 | 仅静态梳理，不执行/不实现 |
| 分享API/密码 | CBC双层padding、success/0、失败统一空 | 仅静态梳理，不联网 |
| 分页/目录链 | 200项上限、400节点、4并发、链/DFS/action差异 | 仅静态梳理，不实现 |
| 文件/302/cache | ID/path差异、sid override、play/mctree/parse缓存 | 仅静态梳理，不执行 |

已执行 Node VM：共 **4768** 向量；向量JSON SHA-256=`4157898e6ed4c45a71a99a8fba83546f4c2dfc9c73cec85d38f87736fff9e6d2`。测试不是把手写期望放进JSON，而是每次即时调用原方法，再对每个向量比较 Rust 结果及错误状态。签名固定 timestamp=`2026-05-14 12:34:56`、随机byte序列 i*17；nonce由原方法生成。

新增文件未注册到 providers/mod.rs，不修改Cargo.toml或共享文件；差分测试通过 path 直接包含模块。Cargo依赖添加、实际运行Rust差分、联网对比、模块路由整合与提交由主agent负责。**在实际 cargo test 通过前，不能将“Node向量生成成功”宣称为“Rust差分通过”。**

遗漏/风险：没有登录、目录、下载的网络验证；AES/RSA仅静态协议分析；URL识别不是安全域名校验，不能据此任意访问解析出的host；null与任意JS对象原型/getter不在JSON模型里；签名入参需保持原JSON.stringify输出，浮点格式与孤立surrogate可能不在Rust String域内；isFolder null应使用try_is_folder；文件ID/path、无分页、树完整性、循环目录、账号无关缓存、不同Authorization规范化、存储全局session覆盖以及源码日志泄密均需后续独立处理，不能本轮顺手改协议。
