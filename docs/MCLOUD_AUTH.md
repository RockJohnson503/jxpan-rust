# 移动云盘扫码与续期离线协议内核

冻结目标：当前部署 `_worker.js` SHA-256
`47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118`。

## 边界

`src/providers/mcloud_auth.rs` 只生成请求、会话和存储写入意图；不会发 HTTP、读写凭据、注册路由或登录真实账号。
时间、UUID、nonce 随机字节和 RSA padding 均由调用者注入。生产包装层需要自己提供真实熵并执行存储 TTL；不要把测试熵用于真实登录。

- `Session::from_uuids`：按原函数移除 UUID 的连字符，构造 deviceId/sid/aesKey。
- `key_request` / `finish_key`：RSA 公钥请求与 skey 封装；上游未返回公钥时保留空 skey（原部署行为）。`stored_value` 供会话存储，原部署 TTL 为 300 秒；`qr_response` 返回二维码链接、300 秒有效期、3 秒轮询间隔。
- `poll_request`：固定版本 7.17.2、clientId 10701、完整原始请求头、AES-ECB + PKCS#7 内层 JSON、原部署签名算法。
- `poll_response`：严格字符串 resultCode，waiting / scanned / expired / confirmed；confirmed 构造 Latin-1 btoa 的 Basic Authorization。缺失存储、缺失会话及异常有独立纯函数响应。
- `prepare_refresh` / `finish_refresh`：以已解码存储值生成续期计划，15 天（毫秒）窗口、失败后 60 分钟退避、XML 转义、原凭据保留、刷新管理标记和时间戳写入意图。transport error 用 None；HTTP 不成功用 `(false, text)`；返回值不执行任何存储加密或数据库写入。

注意原部署使用的 `md5` 不是标准 MD5；这里复用已验证的 `mcloud::generate_mcloud_sign`。授权解码、Unicode/Latin-1 和 XML token 的空白处理也保留原行为，不做“修复”。续期 token 中的 `&amp;` 不进行实体解码。

`mcloud_auth_rsa.rs` 对齐冻结的 `parseRsaPublicKey` 非标准 DER 切片及 `rsaPkcs1v15Encrypt`。RSA padding 中 0 字节替换成 1，而非重新取随机数；大端整数的模幂使用 `num-bigint::BigUint::modpow`，不再维护手写乘法/约减，输出仍左补零至模数字节宽度后编码为 Base64。这不是通用 ASN.1/RSA 库，不用于私钥运算；仅支持最多 512 字节模数/指数，有资源边界；超限、非法 key 返回错误，不对齐原 JS 的所有 malformed-key 错误字符串。

## 离线差分

```sh
node tools/mcloud-auth-vectors.mjs --summary
node tools/mcloud-auth-vectors.mjs --output
cargo test --offline --test mcloud_auth
```

Oracle 先检查部署和 readable.js 的固定 SHA-256，仅抽取并执行**未改写的**纯函数、`mcloud_qrcode` / `mcloud_qr_poll` 原路由块，以及 `jxpanPrepareMcloudCredential`。fetch、storeGet/storePut/storePutRaw 都是严格内存替身；仅允许三个固定端点、两个存储 key 和单次 fetch。存储加密作为边界替身返回 null，以验证原函数的 JSON 后备写入；不运行原 Worker 入口，不访问真实网络或真实账号。

目前 309 组向量：204 AES-ECB、12 RSA、2 DER、6 获取二维码、31 轮询、54 续期。覆盖加密边界长度/Unicode、RSA exponent 1/3/65537 及 256–2048 位模数、nonce 和请求 JSON 顺序、全部扫码状态、数值/string resultCode 区分、缺失会话、异常、毫秒窗口/退避等号边界、XML 特殊字符、BOM/NEL 空白、续期失败保留原授权、写入标记等。

向量 JSON 内容 SHA-256（不含结尾换行）：
`ae6d87e153670e3ee9d3c9a0a08d8a2868b11399562a587b7a2e94c720d3aa64`。

该验证是内核离线兼容证据，不代表真实扫码、后台路由或生产凭据续期已经接通。
