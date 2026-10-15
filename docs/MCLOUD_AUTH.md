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

## 当前官网 QR 入口（2026-10-15）

冻结内核仍保留给离线 oracle。生产 `src/auth.rs` 已独立接入以下入口；不把旧 fixture 通过当作真实登录有效证据，也不全局修改分享解析器的兼容签名。

- `web_key_request` / `web_key` / `finish_web_key`：当前 web 7.18.1、client 10701，数字 `type:1`，标准 MD5 签名；严格检查公钥响应并使用标准 SPKI DER 解码。失败不发布 QR、不保留旧 skey。公钥 bootstrap **不能携带 `hcy-cool-flag`**；此头会让网关按 CBC 处理请求，而 bootstrap 发送的是明文 JSON。
- `mcloud_auth_rsa::parse_web_public_key`：完整 ASN.1 TLV、rsaEncryption OID、BIT STRING、正 INTEGER、模数与指数边界和资源上限检查。旧 `parse_deployed_public_key` 不修改。
- `web_poll_request`：官网 QR `type=5` 实际分流到 `user-njs.yun.139.com/user/thirdlogin`，不是旧 `permission/login`。同一 session 的 SID 同时用于 QR 与 `dycpwd`；固定 web client 的 `pintype=21`、SHA1 secinfo。按明文 JSON 标准签名后，以官网公开协议密钥 AES-256-CBC/PKCS7 封装，携带 `hcy-cool-flag:1`；每次请求独立真实随机 IV。绝不发送现有账号 Authorization/Cookie。
- `web_poll_response_text`：有界解开 outer CBC 与 data 内 hex AES-128-ECB；只接受明确的 root code / `data.result.resultCode` 和准确的 `data.account/token`。known waiting / scanned / expired 才分类；未知错误、数值成功码、冲突代码、畸形确认不伪装 waiting。不递归搜寻 token，账号不接受星号遮罩/空白/冒号，token 内冒号保留。
- `AuthManager`：300 秒、128 sessions、provider 隔离、单 session poll single-flight、epoch 防旧 QR 复活；确认先保存 SQLite 再 hot-swap。后台响应仅返回状态，不泄露 Authorization、token、手机号或 raw upstream error。真实登录状态仅由用户手机确认后更新。

### 已核实主源

官网公开 JS（本轮 GET 与匿名 bootstrap 核对）：
- `https://yun.139.com/w/static/js/app.56b4a805.js`：模块 `7ded`（type 分流）、`e34a`（thirdlogin 明文与解密）、`1690` / `7249`（端点）、`4279`（sign/encryption interceptor）、`4f52`（标准签名/secinfo）、`b441`（web 7.18.1）。
- `https://yun.139.com/w/static/js/chunk-5111af4d.5adeb027.js`：`createLoginQrcode`、`queryQrcLoginResult`，QR SID/device 关联及 scanned/expired/cancelled。

GitHub 原始实现交叉参考，固定 commit 而非随时变化的默认分支：
- `odomu/MoviePilot-Plugins`，commit `df09e93b4f1f0fc3bf4c718ccc6c1795bffbaa2a`，`plugins.v2/cloudsubscribe/drive/yun139/client.py` 的 `create_qrcode_login`、`check_qrcode_login`；用于 web QR 协议互证，不把其宽松 default-waiting/递归找凭据复制进生产。
- `suhuli/cmcc`，commit `f5cc9fb3632aee1912d24894ba20972e4e691d0f`，`src/server/crypto.ts`；仅参考加密/标准签名，未移植其短信或不同 client 参数。

### 本轮验证与边界

- `tests/mcloud_auth_web.rs`：14 项独立 synthetic 测试；保留冻结 fixtures。固定 CBC/ECB 与标准 sign 向量由独立 Node `node:crypto` 生成，覆盖 PKCS7、失败关闭、密钥解析、资源上限与双 session 隔离。
- `tests/auth.rs`：36 项内存网络/SQLite 测试，包含 raw/encrypted 确认、waiting → scanned → confirmed、落库与重建、失败保留旧凭据、精确 TTL、网络中到期、登出/切账号、在途确认与并发只写一次。
- 本地匿名网络探针已实际取得合法 public key，并两次返回 HTTP 200 / waiting；全新随机 SID，未向用户展示二维码、未扫码/确认、未加载现有凭据，内存 DB 的 credential writes 为 0。
- 发布与目标机真实只读回归证据保留在 `.scratch/cloud189-download-qr-8789/` 的独占 JSON 回执；不存在的回执代表未执行，不能以本段文档代替执行结果。

最终真人验收仍需用户在 **8789** 后台新获码并手机确认：waiting → scanned → confirmed，状态显示已登录，并在后续使用中验证授权可用。合成确认测试不是实际扫码成功；匿名 waiting 不是账号授权成功；本轮不代用户扫码、刷新、替换或清空任何真实账号。日志/回执不得保存完整 QR、SID、session key、手机号、token、Authorization 或私有下载 URL。
