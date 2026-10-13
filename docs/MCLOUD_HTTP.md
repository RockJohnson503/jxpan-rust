# 移动云盘只读 HTTP 内核

## 基线与边界

兼容基线为当前部署 SHA-256：
`47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118`。
工具会同时校验 `.scratch/baseline/_worker.js`、`readable.js` 和
`MobileCloudParser.js` 的固定摘要，提取并执行原始 `AESUtils2` 和原始类方法，
不运行 worker 入口、路由、登录或账户刷新。

本内核只提供目录查询和下载地址查询；不访问返回的下载地址，不读取下载文件，
不保存/转存/删除资源，不写配置或凭据，不打印 Authorization、解码账户或响应正文。
复用已经差分验证的 `providers::mcloud` 凭据提取、header 归一化、atob 行为。

## 生产接口

```rust
let http = reqwest::Client::builder()
    .redirect(reqwest::redirect::Policy::none())
    .build()?; // 保持默认 TLS 验证；不要使用 danger_accept_invalid_certs
let mobile = MobileCloudClient::new(http, authorization);
let files = mobile.list_files(link_id, pwd, "root").await?;
let download = mobile.download_url(link_id, path_or_id).await?;
```

- `MobileCloudClient::new(reqwest::Client, authorization: String)`。
- `list_files(&str, &str, &str) -> Result<Vec<serde_json::Value>, MobileCloudError>`。
- `download_url(&str, &str) -> Result<Option<String>, MobileCloudError>`。
- 根目录明确传 `"root"`；显式空目录 ID 保留为空，不自行改成 root。
- `pwd=""` 仍保留 `passwd` 字段，不省略、不传 null。
- `path_or_id` 原样进入 `coIDLst.item[0]`，不自行截取路径或转换 ID；为空时不发请求。
- 未能提取非空账户时返回空列表/None，不发网络请求；无效但非空 Authorization
  按原 pure 模块行为可能提取为 `"default"`，这不是新增登录逻辑。

生产初始 endpoint 固定为 `https://share-kd-njs.yun.139.com`，调用参数不接受 host/URL。
**调用方必须给复用的 reqwest Client 配置 `Policy::none()`，或等价固定 host 的严格策略。**
reqwest 的默认重定向策略允许离开初始 host；传入的 Client 无法由本内核重设策略，
因此“固定初始 endpoint”本身不能替代此调用方要求。内核不会关闭 TLS 验证。
仅 `cfg(test)` 编译中存在 `new_for_loopback` 测试入口，接受 `SocketAddr` 且拒绝非 loopback IP，
没有通用任意外部 URL 注入入口。

## 已实现的部署兼容行为

### AES 双层填充

不是标准单层 AES-CBC/PKCS#7：

1. 计算 JS `plaintext.length`，即 UTF-16 code unit 数量。
2. 手工追加 `16 - length % 16` 个同值字符，包括整块输入时追加 16 个字符。
3. 经过 UTF-8 编码；中文/emoji 的字节数不能用于第 1 步。
4. 使用原分享固定密钥和随机 16 字节 IV，AES-128-CBC 再自动 PKCS#7。
5. Base64 编码 `IV || ciphertext`。

生产 IV 取 OS CSPRNG。公开同步 `encrypt139_with_iv` 仅便于固定 IV 离线校验。
`decrypt139` 先移除 CBC 自动 padding，再按原类最后字节执行手工 slice，
包括不校验手工 padding、负 slice endpoint、TextDecoder UTF-8 replacement/BOM 行为。
纯解密函数在无效密文时返回错误；HTTP 层复现原 AESUtils2 解密失败时返回输入，
因此支持上游明文 JSON 响应。加密失败不会像原 JS 那样降级发送明文凭据：
随机数失败返回错误，这是有意的安全差异。

### 请求

目录：`/yun-share/richlifeApp/devapp/IOutLink/getOutLinkInfoV6`。
下载：`/yun-share/richlifeApp/devapp/IOutLink/dlFromOutLinkV3`。
均 POST 原类全部固定 header 和密文字符串 body。
目录固定 `bNum=1, eNum=200`，没有分页、无限分页或额外请求。

请求明文保留原模板 key 顺序、字段和值类型，不能通过 serde_json 重序列化替换。
替换顺序及 JS replacement string 的 `$$`、`$&`、`$\``、`$'` 语义也保留。
原模板不自动 JSON-escape 输入：含引号/反斜杠的输入可能产生非合法 JSON，
这里不擅自修正，因为当前部署原类同样如此。
Authorization header 采用已验证 pure 归一化，并将 reqwest HeaderValue 标记 sensitive。
HeaderValue 不能表示的输入会返回安全错误，不把错误输入或账户放入错误描述。

### 响应

- 成功判断：JS truthy `success`，或严格字符串 `resultCode === "0"`；数值 0 不算成功。
- `coLst || fileList || files || contentList || []` 使用 JS truthiness。
  空数组为 truthy，因此 `coLst: []` 不会跳到有内容的 fileList；truthy 非数组也不会跳过。
- 正常目录顺序为文件在前，转换后的 caLst 文件夹在后；原始文件 Value 保留。
- 文件夹只输出原类字段：`coID`、`coName`、`coType:1`、`isDir:true`、`path`。
  缺省名称为“未命名文件夹”；缺省路径为 `/` 加原 ID 的 JS 字符串值；
  真正 undefined 的 coID 不添加字段，显式 null 保留为 null。
- 目录 JSON 解析或文件夹转换抛错时，执行原类 catch 救援：先扫描完整 caLst，
  再提取紧凑 coLst；救援顺序为文件夹在前、文件在后。复用正常路径的文件夹归一化，
  `null` 导致 `.map()` 整体失败，不能保留部分转换结果。
- caLst 扫描支持嵌套数组、字符串内括号和引号，但有意保留原类“前一个字符不是反斜杠”
  的引号判定（不会修正连续反斜杠的奇偶规则）。仅在找到完整数组结束后，解析失败才
  使用原 caID/caName 单文件夹正则；保留原始转义、字段顺序、紧凑 `{` 和非空值约束。
- coLst 保留原非贪婪 `]` 正则和其深度 fallback，包括原 `+7` 起点为冒号的偏移问题、
  字符串内括号也计深度的行为；不修复成更宽松的 JSON 恢复器。未闭合数组、不完整对象、
  `fileList` 等其它字段不会被额外救援。没有救援结果时返回空列表。
- 有效 JSON 仍使用原成功判据，不扩大成功判断；只有进入原 catch 的响应走上述救援。
  救援扫描前另有 8 MiB 硬上限，超限返回空列表；每次扫描仅遍历 body 的有限区间，
  不递归扫描、不分页、不发额外网络请求。此上限是安全边界，不声称超限响应与原类等价；
  传输层读取限长仍应由主任务整合，救援上限不能替代 HTTP body 读取限长。
- 下载 URL 优先级：`redrUrl || downloadUrl || url`，不追踪 URL。
  下载 JSON 解析失败（以及原类 `.substring` 因非字符串抛错）时，复现原类
  三个紧凑字段正则救援；该救援有意保留原始转义，不新增空白宽容或 JSON-unescape。
- HTTP 非 200、传输错误以及非法下载响应可返回类型化错误，而原类多把它们吞成空列表/None。
  目录无救援结果和 API 正常拒绝均返回空列表；下载 API 正常拒绝返回 None。
  HTTP 请求 timeout 为 30 秒，reqwest timeout 也覆盖响应 body 读取；原类只在 fetch resolve
  前设置 AbortController timer。

## 明确未实现 / 不声称完整

1. 没有真实账户或公网 live 验证。部署服务状态、凭据有效性和真实下载链接仍需主任务验证。
2. 未实现 tree walk、路由、cache、login/Authorization 续期；这些由主任务另行集成。
3. 未修改 Cargo.toml、providers/mod.rs 或其它 shared 文件；依赖和生产模块导出由主任务接入。
4. 传入 Client 的 TLS、proxy、redirect 策略仍由调用方负责；本内核不构建另一个隐式 client。

此前两项 `unsupportedDirectoryRescue` 已知差异已移除，改为对所有目录向量逐一比较原类结果，
不再用“断言 Rust 未实现”替代兼容验证。当前目录救援语料没有保留跳过或预期差异标记。
这不代表 Rust 验证已经执行通过；最新完整 Rust 验证仍由主任务运行。

## 离线核验

```sh
node tools/mcloud-http-vectors.mjs --summary
cargo test --test mcloud_http -- --nocapture
```

Node 需要私有固定基线源码，非历史源码。Rust 测试运行 Node oracle 后，只对 numeric loopback
发 HTTP。所有账户/令牌/文件/下载 host 均为 synthetic，不使用真实账号，不发公网请求。

当前工具生成 87 个原 AESUtils2 WebCrypto 固定 IV 加解密向量和 91 个原类请求/响应用例，
其中 37 个带名目录救援/负向/正常路径对照用例（包含此前两个救援用例），覆盖：
嵌套括号、字符串内括号与转义引号、完整 caLst/coLst、仅 caID/caName、原单文件夹正则、
单文件正则截断、无救援、JS 空白、null 映射失败、正常/救援结果顺序及明文/AES 两种 framing。
同时保留 ASCII block 边界、中文/emoji、BOM、模板替换、全量 header、空字段、成功码类型、
coLst/caLst/fallback、下载 URL fallback、明文响应、空响应和非 200 向量。
Rust 测试逐一比较全部目录向量，并覆盖实际密文 POST loopback wire、响应转换、未配置账户
短路、测试入口 host 限制、救援扫描大小限制和调用方禁用重定向的 302 行为，没有目录差异跳过分支。

本次 Node `--summary` 和全部 37 项原 VM fixture 断言已通过，语料 SHA-256 为
`a04990c926536ef47ec6248ae8e1c644d9a2539c11d5a10fc3a727c8b8a21973`。
未执行本次 Rust 验证；依赖接入及完整测试由主任务完成，不能仅凭 Node oracle 生成成功
视为 Rust 实现验证通过。原约定依赖为 `reqwest 0.12`（no-default-features，json/rustls-tls）、
`aes 0.8`、`cbc 0.1`（alloc/block-padding）、`getrandom 0.2`；本次救援复用已有 `regex` 依赖。
