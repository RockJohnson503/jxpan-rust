# 天翼登录与续期协议内核

实现：`src/providers/cloud189_auth.rs`。由 `Transport` 与 `Runtime` 注入网络、毫秒时钟和随机数；`ReqwestTransport` / `SystemRuntime` 是可用的生产适配器，构造不会发起网络请求。无路由、数据库、全局 Cookie、后台定时器或线上部署修改。

## 接口

- `init_qr_login()`：unifyLoginForPC 最多五跳，逐域收集 Cookie；HTML 提取 lt/reqId/paramId；备用 open.e 页面及查询参数；生成 reqId/userFinger；getUUID 表单 POST。
- `check_qr_status(qr)`：完整 qrUrl 作为 uuid；UTC 日期、毫秒时间戳；保留冻结部署的请求头行为（不发送 Cookie/user-finger，deviceId 不参与协议）。
- `poll_state(result)`：waiting / scanned / expired / polling 及 exchange 优先级，与原路由分支一致。
- `exchange_session(redirect_url)`：Session、App Token、refreshToken 与 COOKIE_LOGIN_USER/SSON 交换。
- `get_cookie_login_user(redirect_url)`：loginUrl.action 与 callback 两种兜底，最多 11 / 10 次请求；Cookie 值中的 `=` 在本方法保留。冻结 exchangeSession 方法的 `split('=')[1]` 截断行为单独保留。
- `get_open_access_token(key, cookies)`：通过 SsKey 获取 Open Token；保留原始 URL 拼接（不是表单编码）。
- `check_and_login(qr)`：完整轮询 → Session → Cookie 兜底 → Open Token → loginInfo，返回冻结 HTTP 分支的 JSON 信封。Open Token 失败保留 App-only 登录，不进行转存。成功信封含凭据，只能私有持久化/传回授权调用者；普通状态接口不能原样返回。
- `refresh_on_demand(info)`：**使用 app_access_token，而非 refresh_token** 换取 Session，再取 Open Token；两步成功才返回完整替换对象，保留未知字段，失败不产生部分凭据。存储写入由调用者负责。
- `login_status(info, now_ms)`：冻结状态接口的本地元数据判断，秒级边界 `now == expires_at` 仍有效；**不是上游 Token 可用性证明**。
- `validate_login_info(info)`：额外严格结构验证，检查凭据类型/控制字符/长度、Session 成对、Token 至少一种、秒级时间与有效 Open Token 标记一致性。冻结状态接口本身没有这些检查。

## 安全差异（有意不复制原部署缺陷）

1. 只接受精确的四个天翼 HTTPS 主机，无 userinfo/非标准端口；拒绝 IP、相似域名及任意重定向目标。
2. 手动重定向按主机隔离 Cookie。备用登录页跨主机也不发送另一主机的 Cookie。原 getCookieLoginUser 将累积 Cookie 向所有跳转目标转发；不复制这一行为。
3. 默认 fetch-follow 跳转由专用 reqwest 池手动实现，仅允许同主机；有界跳转与 20 秒超时，响应体流式限额 2 MiB。不使用共享 Cookie jar 或默认身份头。
4. 传输/JSON/限额/不安全 URL 等异常返回静态 `AuthError`，不回显上游响应、凭据或错误 URL；不复制原异常字符串暴露行为。等待等正常上游状态仍按原响应判断。
5. `status == 0` 但没有 redirectUrl 的异常响应拒绝为 InvalidQr；不把 JavaScript undefined 拼进交换请求。
6. Cookie 适配器须保留多条 Set-Cookie，不依赖逗号拼接（Expires 自身含逗号）。删除 Cookie 的过滤、同名替换顺序及值内 `=` 行为有离线覆盖。

## 冻结原函数离线差分

冻结 `_worker.js` SHA256：`47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118`。

`tools/cloud189-auth-vectors.mjs` 在无真实网络的 VM 中执行原 Cloud189Parser、原 jxpanCloud189RefreshOnDemand、原安全 JSON 解析，以及通过 AST 定位的原 cloud189_check/cloud189_status 分支。只使用 synthetic 凭据；每个请求必须消费预设响应，未预设调用立即失败。输出包含原函数哈希、原返回值、完整 URL/方法/请求头/表单/redirect 轨迹、模拟存储写入。Rust 集成测试逐字段比较，异常病例比较已记录的安全差异。

```sh
node tools/cloud189-auth-vectors.mjs --summary
cargo test --test cloud189_auth
```

当前 54 个冻结差分病例，另有不安全 URL、跨主机 Cookie 隔离、续期不修改输入、严格验证与有效期边界测试。未进行真实扫码、真实 Token 续期或凭据持久化；路由权限、QR 保存/过期、原子落盘和续期单飞由上层接入负责。
