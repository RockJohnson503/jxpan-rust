# 前端兼容改动

在原部署导入的 `web/admin.html` 和 `web/index.html` 上做授权/日志接口兼容及安全修正，并提供本地 QR 编码器、许可证/来源说明和合成验证。延续原页面视觉与目录树控件，账号区域统一卡片/详情，不声称 HTML/CSS 与旧版字节一致，也未对其它区域作大规模格式化。`admin-login.html` 保持原字节；静态登录文档 referrer policy 和原生 POST 的 Origin 校验修复位于服务端，详见 [AUTH](AUTH.md)。

当前 Rust 已正式接管 8788，部署、独立 final audit 与精确临时文件清理已核验，正式入口页面/QR hash/匿名私有接口 401 均通过；Git 提交推送按独立步骤核验，结果以 Git 历史/回执为准。本文保留页面实现与测试边界，不将端口或静态资源验收泛化为真人扫码或完整下载通过，发布指纹和回执见 [TEST_DEPLOYMENT](TEST_DEPLOYMENT.md)。

## 当前统一“网盘账号”界面（2026-10-15）

- 侧栏、概览和账号管理页统一使用“网盘账号”；天翼/移动的状态卡、扫码按钮、详情使用一致展示模型。只支持这两家；其它网盘显示“暂不支持”，不把保留的旧 JS 函数或控件当作 Rust 功能已迁移，对应未实现服务仍为 `success:false/code:501`。
- 状态仅分读取中/失败、未授权、已配置、授权待续期；“已配置”只是本地存在授权，不代表在线验证。“刷新状态”只读取本地安全状态，不调用供应商、不创建 QR，也不刷新凭据。
- 卡片、管理页、详情一致展示状态、白名单授权来源、已脱敏账号、授权时间、过期时间及续期说明。未知来源/畸形时间/非掩码账号不原样回显；缓存只保留安全展示模型，不保留原始状态对象或凭据。字段经 `textContent` 输出，不展示调试 JSON、原授权/手机号、供应商原始错误，也不向 console 打印请求/响应。
- 明确提示“自动续期始终开启，解析时按需尝试”“根授权失效时可能需要重新扫码，续期不保证成功”。本地到期显示“授权待续期”，不是立即断言必须重新扫码。无用户可关闭的续期开关，也不是后台定时保活；移动 provider 禁用状态不因这一文案或续期策略而自动开启。
- 这些页面/安全断言已在 synthetic VM 中覆盖；真人扫码确认、真实长周期续期、完整浏览器视觉/下载矩阵仍未验收。天翼 S005/S006 目录内文件已取址 302，但该后端证据不证明目录 ZIP、CDN 正文或前端完整下载链已通过。

## 天翼 / 移动授权

- 后台天翼已有扫码卡接入新接口；移动卡原“暂未实现”区域替换为同风格的扫码按钮、二维码/倒计时/取消区域，并补充注销按钮。
- 首页保留原扫码弹窗，补充移动扫码按钮。天翼/移动原“保存到内存”按钮改为“保存到服务端”；其它提供者的原内存配置功能保留。
- 二维码创建、检查、手动保存、注销均使用 `POST /`、`URLSearchParams` 请求体和浏览器普通 `same-origin` cookie。凭据和扫码 session ID 不出现在这些请求的 URL 中。
- 支持动作：`cloud189_qrcode`、`cloud189_check(device_id)`、`cloud189_manual_save(access_token)`、`cloud189_logout`；移动对应 `mcloud_qrcode`、`mcloud_check(device_id)`、`mcloud_manual_save(authorization)`、`mcloud_logout`。
- 二维码创建读取 `data.qr_url/device_id/expires_in/interval`。检查只依赖 `data.status`；`success`/`confirmed` 视为服务端已自动持久化，不再读取或复制 `loginInfo`、access token、Authorization。`success=false` + `expired` / code 400 会停止轮询。
- 两页同源加载 `/assets/qrcode.js`，天翼/移动的 `qr_url` 由浏览器本地 `qrcode(0, "M")` 编码为 GIF data URL，原 180/200 像素显示尺寸保持不变；本地库缺失/生成失败时停止会话并提示，不回退第三方二维码服务。保留的其它提供者只更换二维码图片生成方式，其动作、轮询、存储及布局不变；移除所有 `api.qrserver.com` / Google Chart QR 源，避免任何扫码会话外发。
- 编码器是官方 npm `qrcode-generator@2.0.4` 的 `dist/qrcode.js` 字节一致副本，MIT 完整授权保存在 `web/vendor/qrcode.LICENSE`，精确来源、npm integrity、Git commit 和 SHA-256 见 `web/vendor/README.md`。页面显式使用库自带 UTF-8 编码，保留二维码 quiet zone。工具端依赖精确锁定、安装不执行 scripts；生产运行仅需 vendored 文件及同源静态路由，不需 npm/CDN。
- 两个提供者的轮询采用异步完成后再安排下一次 timeout，不重叠发送检查；每个活动会话最多一个 countdown interval 和一个 polling timeout。总期限最长 300 秒，隐藏浏览器标签、后台切离扫码页、取消、失败/过期、成功均清理计时器并 abort 请求；旧请求返回不能覆盖新会话或恢复已取消会话。
- 手动保存成功后清空输入，不复制天翼/移动凭据至浏览器内存。首页移动 Cookie 原控件保留，提示当前服务端授权接口只保存 Authorization。
- `GET /?action=login_status`、`GET /?action=cloud189_status` 只读本地安全状态，对供应商请求为 0。前端只保留并用 `textContent` 展示白名单元数据，不展示 `loginInfo`、cookie/token、原账号或调试字段。
- 尚未接入 Rust 的提供者不因返回伪登录状态而显示可用；当前页面统一显示“暂不支持”。天翼/移动配置状态标注“未在线验证”，不把配置存在等同于在线验证通过。
- 保留的其它提供者授权函数也改成表单 POST，避免旧凭据保存/登录/注销/扫码调用发送敏感 URL；这些函数的保留不代表 Rust 授权或解析已迁移。

授权写操作需要先在同源浏览器登录后台。页面会显示服务端拒绝/获取失败，不添加 URL admin token 或硬编码凭据。

## 日志保留期

后台“解析记录”页增加小型数字输入、保存按钮和提示：

- `GET /admin/?action=get_log_settings` → `{code:200,success:true,data:{log_retention_days:7}}`。
- `POST /admin/`，body `action=set_log_settings&log_retention_days=<整数>`。
- 前端校验范围 1–3650，初始读取成功前禁用输入/保存，读取失败保持禁用并提示刷新重试；防止重复读取或重复保存。
- 缩短天数前必须确认“立即永久删除超期解析记录，无法恢复”；取消确认不会发送请求。服务端保存成功后刷新统计和记录，失败显示错误并可重试。
- 原单条删除、分批删除、取消清理也改为表单 POST；原记录展示/搜索/分页保留。导入的原页面未包含独立 `reparseRecord` 函数/重解析按钮，本次不凭空添加功能；原首页解析入口未移除。
- 后台会话退出同样使用 `POST /admin/`，body `action=logout`，成功后导航至 `/admin/`；服务端需接受该同源已授权表单动作。

## 收尾审查：记录安全与搜索分页

- 后台记录详细 JSON 先 `JSON.stringify` 再 `escapeHtml`，阻断文件名、错误文本及嵌套对象中的 `</pre>`、SVG/script 等存储型 XSS。
- 同一展示链的删除按钮参数先编码为 JSON 字符串字面量，再转义 HTML 属性；记录请求错误和通知也转义。搜索高亮改为分割原文本、逐片转义后加固定 `<mark>`，不在已经转义的实体中替换，避免破坏 `&amp;` 等。
- 普通列表和搜索共用 `loadRecords`。搜索请求明确携带 `action=search_records`、`search`、`type=currentTab`、`page`、`limit=10`，由现有后端 SQL 在分页前过滤，而非无界拉取两个 tab 后在页面筛选。计数使用后端 `successTotal/failedTotal`，不是当前页长度。
- 搜索结果保留原分页按钮；翻页、刷新保留关键词和页码，不再通过 `searchRecords` 强制跳回第 1 页。输入新词、清空关键词或切换 tab 才重置第 1 页。过时请求成功/失败返回均不能覆盖最新结果。

## 收尾审查：移动文件直链

- 无预加载直链时，移动单文件按钮请求现有普通 JSON 解析入口：`?url=<share>&pwd=<pwd>&id=<fileId>&type=json`。根目录文件无需 fid 链；有嵌套链时按原页面惯例传 `fid/fid2/fid3/...`，不再依赖旧 `parse_mcloud_subfolder&file_id` 分支丢失文件 ID 的行为。
- 生成短链的下载目标保留相同文件 ID、密码及完整 fid 链（`type=down`）。预加载直链分支保留原行为。仅对该分支的错误/异常 HTML 增加转义。
- 服务端已接旧移动动作的文件 ID 修复及 `/assets/qrcode.js` 静态 GET/HEAD 路由；当前 HTML 布局不因接线重写。

## 2026-10-14 实机反馈：解析按钮重复请求

- 原导入按钮同时带 `onclick="parseLink()"`，又由 `setupEventListeners` 注册 click listener；一次激活调用两次 `parseLink`。真实 Chrome 使用拦截的 synthetic fetch 确认两个调用栈，未向后台发送诊断解析请求。
- 仅移除该按钮的 inline onclick，保留既有 JS listener、`type="button"`、参数编码、URL 自动解析及布局/CSS；不新增去重窗口，也不改变后端请求/缓存统计或历史写入规则。
- 回归模拟同一个静态按钮的 inline handler 和注册 listener，先在旧页面上失败、修复后通过；覆盖一次点击一请求、稍后主动重试、原 URL 自动解析及其后的手动点击。不能只直接调用 `parseLink` 或只模拟 JS listener。
- 用户授权后已更新独立 8789 测试实例；新页面真实 Chrome 一次点击仅发起一个被拦截的合成解析调用。升级和诊断没有重置或回扣已有统计，也没有新增真实解析记录；部署/备份指纹见 [TEST_DEPLOYMENT](TEST_DEPLOYMENT.md)。

## 验证

运行：

```sh
node tools/verify-frontend.mjs
```

脚本使用现有 `tools/node_modules/acorn`，解析两页全部 inline JavaScript 及 HTML 静态事件处理器，再在 Node VM 中执行原 inline 脚本和真实 vendored QR 编码器（不是 QR stub）。DOM、时钟和 fetch 全部为合成实现，动态图片 `src` 也被捕获并断言只能是本地 GIF data URL；输入仅使用显式 synthetic 占位数据，不会发送网络请求、加载公网 QR/CDN，也不执行旧服务写操作。

合成行为覆盖：

- 两个页面 × 天翼/移动的正确表单动作/字段、cookie 参数、成功自动持久化响应、false/400 过期、最长 5 分钟、隐藏/取消、串行慢轮询、重复启动、旧响应忽略及 abort；手动保存/注销及输入清理。
- 安全详情/缺失提供者；日志读取、整数范围、缩短确认/取消、延长、失败重试以及单条记录删除 POST。
- 实际本地 QR factory、GIF 头尾/尺寸及矩阵 smoke test、`createDataURL/createImgTag` API；全部 legacy QR 图片本地生成、Unicode 编码、无原 session 插入 HTML/remote fallback；缺失本地库时失败关闭；旧 QR 响应不会渲染额外会话。校验 vendored JS 和完整许可证 SHA-256。
- 后台搜索当前 tab/type/page/limit、成功/失败搜索独立总数与分页、翻页/刷新、新词/清空、切 tab、旧结果/旧错误忽略；详细嵌套 JSON 存储型 XSS、URL/密码/消息与搜索高亮、删除 handler 字符串边界、搜索空结果及服务端错误转义。
- 移动根文件/嵌套文件的普通解析 URL、完整 ID/密码/fid 链、成功 UI/短链目标、预加载直链保留，以及失败/异常 HTML 转义。
- 统一账号 UI 的卡片/详情一致性、读取中/失败/未授权/本地到期状态、来源 allowlist、掩码账号/时间校验、安全模型缓存、旧响应忽略，以及无调试日志/原始 JSON/凭据回显。强制按需续期文案不误报为供应商已验证或必须立即重新扫码。

当前已核验：**PASS，6698 项前端 synthetic checks**；全项目另有 351 项 Rust、44 项部署工具测试、9 项 verifier safety 测试、fmt 及 Homebrew Rust 1.93.1 严格 Clippy 通过。收尾再次运行 Rust/前端/隔离 verify-pages/9 项 verifier safety/fmt/严格 Clippy 全部通过。前端计数不等于真实浏览器/真人扫码用例数量；更早的 2888/3086 是各自历史窗口，不当作最新结果。

VM 脚本边界：不是完整浏览器视觉/扫码解码/端到端测试；不访问真实网盘或生产服务，也不请求 `/assets/qrcode.js` HTTP。静态资源字节/hash/MIME/nosniff、隔离后台 cookie 和私有头已由 root HTTP/页面测试另行验证。其它既有 Tailwind/字体/Chart/Forge 等 CDN 引用保留，没有引入新框架或修改布局/CSS。

历史另行完成的真实 Chrome smoke（合成管理员、临时 DB、无真实供应商账号）：首页渲染/公开零统计、原生登录、后台和记录页、默认 7 天、UI 保存为 8 天并重载显示 8 天、首页/后台本地 GIF QR 工厂通过；未请求供应商 QR 创建/轮询，也未删除历史。当时的禁刷新配置不是当前可用的开关。修复前真实原生登录出现 403，补 Origin/Fetch Metadata 拒绝和合法路径回归后复测通过。退出 UI 的确认框因当时工具不支持 handle_dialog 未继续，不能报告浏览器退出成功；loopback 会话退出/失效回归已通过。临时服务和工具新建 tab 当时均已回收；该历史 smoke 不是本轮完整浏览器矩阵、真人扫码、真实供应商持久化/续期或性能验收。
