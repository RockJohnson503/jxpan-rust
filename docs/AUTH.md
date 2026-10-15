# 天翼 / 移动授权生命周期组件

> 当前 Rust 已正式接管 8788，部署、独立 final audit 与精确临时文件清理已核验；Git 提交推送按独立步骤核验，结果以 Git 历史/回执为准。本页描述当前代码行为，正式端口及静态 QR hash 验收不等于真人扫码/真实长期续期通过。制品指纹、数据交接和审计回执见 [TEST_DEPLOYMENT](TEST_DEPLOYMENT.md)。

## 接口与当前集成边界

`src/auth.rs` 提供：

```rust
AuthManager::new(storage: Storage, http: reqwest::Client, initial: Value).await
    -> auth::Result<AuthManager>
manager.snapshot() -> Arc<Value>
manager.revision() -> u64
manager.versioned_snapshot() -> (u64, Arc<Value>)
manager.handle(action: &str, params: &http::params::Params, allow_secrets: bool).await
    -> Option<Value>
manager.refresh_cloud189().await -> auth::Result<bool>
manager.refresh_mcloud().await -> auth::Result<bool>
```

本组件不读取任意文件路径；`initial` 是调用者已经从**私有配置文件**读取的配置。只负责天翼与移动，不实现其它网盘。初始化只读取存储、配置和环境变量，不发送网络请求。`snapshot` 是服务器内部私密快照，**不得整体发送到前端或日志**。

`auth` 已由 `src/lib.rs` 导出。`http::AppState::new` / `from_env` 创建一个 `AuthManager`，与管理员会话、解析历史和短链共用同一个 `Storage`；克隆 AppState 共享授权状态及 HTTP 连接池。`http::router` 已在 `/` 和 `/admin` 接入 `handle`：公开 `login_status` / `cloud189_status` 只返回脱敏状态，其它授权动作要求后台登录及通过同源检查的 POST；组件仍以 `allow_secrets` 再次防御。

### 原生登录表单与同源检查

`web/admin-login.html` 的原 HTML/原生 POST 保留。本次真实 Chrome 发现：若登录文档使用 `Referrer-Policy:no-referrer`，其非 CORS 原生 POST 会发送 `Origin:null`，旧同源校验误拒为 403。私有 API/已登录后台仍为 no-store/no-referrer/frame DENY、无 CORS；**只有静态登录文档**使用 `same-origin` referrer policy，禁止向外站发送 referrer，且普通 HTTP LAN 无 Fetch Metadata 时也能提交合法 Origin。

隐私策略仍可能使 Origin 为 null，因此该值只在 `Sec-Fetch-Site:same-origin` 时通过；缺失、same-site、cross-site、none 或未知 metadata 都不足，错误/外站 Origin 即使附同源 metadata 仍拒绝。不泛化接受所有 null origin，不以配置同一 AppState 代替请求边界。`tests/common_http.rs::native_login_origin_and_fetch_metadata_boundaries` 同时覆盖上述拒绝表、null+精确同源元数据的合法路径以及合法 Origin+无元数据路径；页面验证工具保持私有/静态登录头的分别断言。

规范参考：WHATWG Fetch Standard §3.2 Origin header / append a request Origin header (`https://fetch.spec.whatwg.org/#origin-header`)，W3C Fetch Metadata Request Headers §2.3 Sec-Fetch-Site (`https://www.w3.org/TR/fetch-metadata/#sec-fetch-site-header`)，W3C Referrer Policy §3.3 same-origin (`https://www.w3.org/TR/referrer-policy/#referrer-policy-same-origin`)。本地 Chrome 已通过原生登录与保留期 7→8 保存/重载，不等于真人扫码或现网后台已验收。

天翼 AuthKernel 已提供 `ReqwestTransport::with_client(reqwest::Client)`，`AuthManager::new` 使用 `with_client(http.clone())`；天翼授权、移动授权与当前分享解析复用 AppState 的共享 reqwest 池。独立的 OAuth owned-directory 客户端不属于这条共享接线，也没有被加入公共解析流程。

传入 HTTP Client 必须使用 `redirect(Policy::none())`，没有自动 Cookie jar 或带授权的默认头。移动只请求 kernel 的三个固定 HTTPS URL，拒绝 3xx；响应后的 URL 校验不能替代请求前禁止自动跳转，因为带秘密的 POST 可能已发出。天翼使用 kernel `allowed_url`、host-scoped cookie、同主机跳转规则。生产 transport 每请求 20 秒总超时、响应最多 2 MiB（Content-Length 与流式累积均限制），错误只返回本地固定信息。

## 配置与持久化

优先级：存储行 > 私有 `initial` seed > 环境 seed。兼容 key：

- 天翼：`c189_login_default`，对象或历史 JSON 字符串。
- 移动：`mcloud_login_default`，对象、历史 JSON 字符串或历史 Basic authorization 字符串；支持对象 `auth` alias。

使用已有 Storage 的加密 KV API，组件不增加自定义加密。旧公开固定 AES key 不是应用级保密机制，SQLite 文件仍需私有文件权限。

支持 initial 的上述 key、`cloud189.loginInfo` / `login_info`（对象或 JSON 字符串）、`cloud189.accessToken`，以及现有私有文件的 flat `cloud189.open_access_token` / `app_access_token` / `access_token` / session 字段；`mcloud.loginInfo` / `login_info` / `authorization` / `auth`。空 loginInfo 当作未配置。flat 天翼 seed 缺少时间时初始化 30 天本地元数据，不宣称供应商验证；持久行仍要求完整合法时间。快照同时保留 `loginInfo` 和现有解析器需要的 flat provider credential 字段，手动切换/注销清理旧 session/cookie/token alias。有效移动 authorization 必须能解码为非空 `prefix:account:token`；无配置、空值或错误数据不伪造登录成功。损坏的存储行直接初始化失败，不静默恢复 seed。

源码 `MCLOUD_AUTHORIZATION`（兼容 `MCLOUD_AUTH` / `mcloud_auth`）、`MCLOUD_ENABLED`、`MCLOUD_USER_AGENT` 可从 initial 的配置键或环境读取。天翼支持源码 `P189_TOKEN` / `p189_token` / `CLOUD189_TOKEN` / `cloud189_token` seed。nested initial 配置优先于环境 seed。`enabled` 和 `userAgent` 保留在快照给解析器使用；协议 kernel 的固定签名 UA 不在这里覆盖。源码 `MCLOUD_COOKIE` 不是扫码授权 token，本组件不把仅有 cookie 视为登录成功，也不把它转换为 Basic authorization。

每次保存：校验 → provider 异步提交锁 → epoch CAS → Storage 持久化 → 内存快照交换 / revision 自增。网络阶段不持有任何 std 锁；提交锁不跨网络操作。提交一旦进入，独立 Tokio 提交任务完成持久化和热更新，即使 HTTP 等待者断开，也不会造成“数据库已改、进程仍使用旧授权”的取消窗口。提交任务无网络请求、无周期后台服务。

注销在同一兼容 key 持久化 `{"_jxpan_logged_out":true}`，而不是删除该 key。这保证重启不会自动恢复私有文件或环境 seed。重新扫码/手动保存覆盖该退出标记。注销 / 手动切换清理该 provider 的 QR；另一个 provider 的会话不受影响。

快照持有者看到不可变旧版本，新的调用者取得新 Arc。解析器不要永久缓存最初快照；`versioned_snapshot` 在同一锁下返回 revision 与凭据，避免账户和缓存版本混配。AppState 按该版本生成私密输入的 HMAC 缓存键；授权保存/注销后清空解析、下载及目录索引缓存，旧在途请求只能使用自己的旧账户/旧版本，不能填入新账户的缓存键。`revision` 是当前进程的版本计数，重启归零，不是数据库事务号。只支持一个 AuthManager 管理其 credential keys；多个独立实例/其它进程并发修改同一 key 不在进程内 epoch CAS 的保证范围。

## 动作

| action | 参数 | 权限 |
| --- | --- | --- |
| `login_status` | 无 | 可公开；只返回 mask 元数据 |
| `cloud189_status` | 无 | 可公开；只返回元数据 |
| `cloud189_qrcode` / `mcloud_qrcode` | 无 | 授权 |
| `cloud189_check` / `mcloud_check` | `device_id`，由二维码响应提供的 opaque session UUID | 授权 |
| `cloud189_manual_save` | `access_token`，或完整 JSON `login_info` | 授权 |
| `mcloud_manual_save` | `authorization` | 授权 |
| `cloud189_logout` / `mcloud_logout` | 无 | 授权 |

不支持的动作返回 `None`。这些名字是组件接口，不是对旧源码 `mcloud_qr_poll` / `mcloud_qr_save` 的隐式别名；UI 调用者须适配动作名称与 opaque `device_id`。

`allow_secrets=false` 除 public status 外一律返回 403，既不能建立/轮询会话，也不能写凭据或注销。调用者应先做管理员权限判断，组件再次防御。状态即使 `allow_secrets=true` 也不返回完整 token；扫码确认的 loginInfo / authorization 在服务器内部保存后剥离，UI 只需状态。公开状态的 `verified:false` 表示本地配置元数据，不宣称供应商已验证 token。

天翼手动 `access_token` 保存为 Open token、30 天本地到期；完整 `login_info` 应遵守 kernel 严格校验（合法 token、配对 session key/secret、正数秒级 login_time/expires_at）。移动 timestamp、刷新失败时间和 token 内 expiry 使用毫秒。签名时间遵循源码 runtime-local `YYYY-MM-DD HH:mm:ss`，默认 UTC+08，可通过 `auth_timezone_offset_seconds` 显式改变；不自动猜测服务器时区。

## QR 与刷新

- QR ID 使用 OS 熵 UUID v4，TTL 300 秒；`now >= expiry` 返回 expired。最多 128 个会话，访问时清理过期项，不启动清理守护任务。
- 天翼复用 `AuthKernel::init_qr_login` / `check_and_login`；移动复用 Session、key request、RSA、poll request / response。移动建立二维码必须取得真实有效公钥；缺 key / 失败不会伪造成功。测试中该请求完全由内存 transcript 替代。
- 单个 QR 的 poll 用 Tokio 锁串行化，成功持久化后删除自己的 session；其它 provider 的 session 保留。扫码失败或交换失败保留旧凭据，失败 session 可继续检查。provider epoch 阻止注销或手动切换后在途旧 QR 恢复旧授权。
- 对外 poll 只输出固定状态及固定消息，剥离源码返回的 raw_status、上游错误、loginInfo、authorization 和手机号，防止供应商错误文本回显秘密。expired 统一返回 400 / false；与源码移动“HTTP envelope code=200 / success=true”的 expired 响应存在有意差异。
- **2026-10-15 强制按需续期**：生产和嵌入式解析/取址路径不再存在 `refresh_auth` 或环境 opt-out；源码不读取 `JXPAN_AUTH_REFRESH`，历史环境即使为 `false` 也不能关闭续期。仅使用已登录、启用的受管账号时检查；初始化、`login_status` / `cloud189_status`、首页和匿名后台验证对供应商的网络请求为 **0**，不做在线探活。禁用整个移动 provider（布尔或字符串 `false`）仍禁止解析及续期；强制续期不等于自动启用 provider。
- 天翼在缺 session/open token 或本地到期时先尝试 app token → session → open；不再要求旧 session 必须存在。两家在确凿认证拒绝且本次业务失败后，自动续期并用同一账号的新凭据**最多重试一次**。天翼信号为 HTTP 401 或既有认证业务码白名单；移动当前只识别 HTTP 401，未核实的业务码、403、密码/分享错误、畸形响应和普通网络故障不冒充认证到期。成功的共享只读 fallback 不触发拒绝恢复。
- 移动主动续期保留到期前 15 天窗口及失败 1 小时退避；明确认证拒绝可跳过有效期窗口，但不绕过失败退避。两家成功续期均有持久化的 60 秒冷却，防止同 token/短有效期回复在每个请求重复刷新；冷却期间仍遭明确拒绝返回暂时 503，不立即要求重新扫码。
- 有界 single-flight cache 按 provider+epoch+触发原因隔离（无 credential），两个原因再由 provider mutex 串行化；60 秒、16 entries、4 KiB、2 active load。请求、刷新及一次重试共用 120 秒期限；最终 history/stat 只处理一次。
- 网络结果经 epoch CAS 校验、先持久化再热更新；失败不清空旧凭据。请求另持登录 generation 租约，manual/QR/logout 使旧租约失效，续期不切换账号。外部天翼请求 token、无受管账号时的移动 auth 不触发受管续期。续期后的响应不留在旧 revision 缓存键，旧 revision 不发布目录索引。
- 强制开启不是绕过供应商：缺少可续期凭据、上游撤销/拒绝续期仍可能需要用户扫码。没有后台定时保活，闲置期不自动请求供应商；真实长周期到期续期尚未验收，离线/内存测试不等于真实账号续期成功。

## 安全差异与验证

旧 frozen `login_status` 返回原授权 / loginInfo，部分代码还打印 token/请求响应。本组件有意不复制这些漏洞：public status 只提供是否配置、到期、session/open token 是否存在、时间、source allowlist、掩码账号；不记录请求、响应、凭据或上游 error 文本。原始 `snapshot` 仅供服务器内部使用。

运行：

```sh
cargo test --locked --offline --test auth
# 另行运行 HTTP 接线回归；仅 loopback，不访问供应商。
cargo test --locked --offline --test common_http
```

上述回归不进行真实供应商验证：没有真人扫码、没有真实 RSA key 请求、没有真实供应商 HTTP，没有真人手机号/授权。`tests/auth.rs` 以 SQLite `:memory:` + 注入 AuthNetwork transcript + 固定可推进时钟执行真实组件状态机，使用现有 frozen Mobile fixture 的合成 RSA 公钥；生产 QR/解析路径并未因此关闭。隔离依靠不加载真实账号和注入内存网络，而非已移除的刷新开关；`verify-pages.py` 默认 synthetic、无 provider 账号。`tools/verify-live.py` 加载真实私有账号，须显式传入 `--allow-provider-side-effects` 并事先获得相应目标/供应商副作用授权；`JXPAN_AUTH_REFRESH=false` 无效，不得当作禁续期的只读验证。现有 kernel 的差分 fixtures 仍由其各自测试维护。

覆盖：空状态、权限 gate、手动持久化/不可变旧快照/热切换、重启优先存储、两 provider 注销防 seed 恢复、成功 QR 保存与重复 poll expired、保留其它 provider session、准确 300 秒过期、provider mismatch、交换失败保留旧凭据/会话、失败信息剥离、授权 mask、移动续期成功/失败/退避、天翼续期成功/失败、single-flight、并发手动切换 CAS、legacy 行读取、损坏存储 fail-closed、会话容量限制及回收、Storage 关闭后保存失败、构造函数不发请求。

**已接入不等于真实业务验证**：共享 Transport constructor、lib 导出和 HTTP/解析器快照接线已完成；`tests/auth.rs` 通过根 crate 导出测试组件，`tests/common_http.rs` 覆盖 HTTP 权限、脱敏状态、手动保存/注销与重启持久化。本轮主代理已核验全项目 351 项 Rust、6698 项前端 synthetic checks、44 项部署工具测试、9 项 verifier safety 测试、fmt 及 Homebrew Rust 1.93.1 严格 Clippy，但不能将全项目计数视作授权组件的真实供应商验收。天翼 S005/S006 目录内文件取址已有 302 正例；CDN 正文/播放、目录 ZIP、真人扫码、真实长周期续期、转存及性能闭环仍未验收。OAuth 凭据与扫码 SessionKey 的同账户身份绑定也未验证，见 [owned 能力边界](CLOUD189_TRANSFER_CAPABILITIES.md)。真实供应商操作必须在明确授权范围内进行，不能把合成 transcript 或 loopback 通过当作线上成功。
