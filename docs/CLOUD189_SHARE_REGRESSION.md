# 天翼分享兼容与真实回归（2026-10-15）

## 历史窗口与当前边界

本文记录 **2026-10-15 `test-3d1cee097916` 的历史 8789 窗口**：当时仅更新 8789，未替换、重启或修改原 8788，修复分享 metadata / 目录大小占位值、公开分享附带无关提取码、Open MD5 前导零和认证错误分类。下方测试数、镜像、未取到地址、旧容器留存及未提交记录均只描述该窗口，不代表当前版本/库存。

**当前 Rust 已正式接管 8788，部署、独立 final audit 与精确临时文件清理已核验**，使用 `jxpan-rust:release-f7496a9c10e0`；Git 提交推送按独立步骤核验，结果以 Git 历史/回执为准，详见 [TEST_DEPLOYMENT](TEST_DEPLOYMENT.md)。后续 **S005/S006 目录内文件取址已从 400 恢复为 302**，不能再概括为当前全部下载地址均认证阻塞；302 不等于 CDN 正文/播放、目录 ZIP 或完整下载矩阵成功。

该历史窗口的 Open 请求为 `AccessTokenHasExpired`、App 为 `InvalidSessionKey`，修复后受控返回 401 / `cloud189_auth_required` / `need_relogin:true`，当时没有助手登录、扫码、刷新或替换凭据。旧“需在 8789 重新登录后补正例”是当时下一步，不是当前必须先完成的部署动作。当前强制按需续期无环境关闭开关，真人扫码及真实长期续期未验收；真实验证须先获供应商副作用授权并显式使用 `verify-live.py --allow-provider-side-effects`，页面验证默认 synthetic、无 provider 账号。

## 原版与开源实现核对

- 继续以本地冻结 `Cloud189Parser.js` / worker 为原版协议和缺陷依据；原 oracle / fixture 不重写。
- 核对 OpenList 的 PC 会话签名与错误分支：`OpenListTeam/OpenList` commit `482464c2affc5eeba5d4a25bb272aeb352ec5918`，`drivers/189pc/utils.go`。
- 核对 `xxcheng123/cloudpan189-interface` commit `b552a0a73f414994b582e5c3b465a82157f87703` 的 `client/lib.go`。其 TV/Open 请求有自己的 AppKey 等参数，不将整套 TV 参数直接套入当前 PC/Open 分享接口。
- 上述源码仅作参考；是否适用以实际同参数对照、生产代码回归和现有授权边界为准。没有引入第三方登录、转存、删除或 owned 下载逻辑。

## 本次运行时代码差异

相对上一部署 `test-e897dd1e919a`，运行时仅三个文件改变：

1. `src/providers/cloud189_client.rs`
   - 接受数字/字符串成功码 `0`，保留经过验证的 `shareMode` 与 `needAccessCode`。
   - nested metadata 的 shareId 可使用外层字段；目录 size 占位值规范为 0，普通文件大小仍严格验证并保留大整数精度。
   - 仅在明确 `needAccessCode:false`，或实际验证的 **mode=2 + 非空 shareId + 显式空 accessCode** 形状下认定不需要提取码，不把所有 mode=2 或所有错误码都当成公开分享。
   - Open 网络目录/下载签名使用固定 **32 位 MD5**；兼容模块中的 `java_md5` 及冻结历史签名 oracle 保留旧行为。
   - metadata / 密码 / Open / App 下载将已知认证、失效分享、提取码错误分类；不反射上游原始消息、凭据、文件 ID 或签名 URL。
2. `src/providers/cloud189_service.rs`
   - 密码检查仅在已明确不需要密码时跳过；受保护/未知状态仍验证，失败不继续请求下载。
   - Open → AppShared（普通）和 AppShared → Open（fast）仍允许另一路成功，全部失败时保留安全错误分类，认证失败优先返回明确 401。
   - 常规单文件、file action、folder-item 文件和 fast 分支接入分类错误。
3. `src/http/reply.rs`
   - fast 错误保留受控 400/401/403/404/413 的 HTTP / JSON code；其它失败仍 502，避免 service 401 又在 HTTP 包装层被改回 502。

没有增加 Web/VLC/owned 下载路径，没有改动分页总量规则、成功路线固定策略、安全预算、缓存 TTL、移动 provider 或前端源文件。TreeError::Upstream 的目录错误尚未全面保留供应商类型；本次并不保证所有未见过的上游失败都已细分。

## 实际签名与公开分享证据

- 同一个真实文件、share、token 和时间戳，旧 MD5 前导零被裁剪至 **31 位**时，上游 HTTP 400 `InvalidSignature`；保留 **32 位**时变为 HTTP 400 `AccessTokenHasExpired`。这证明该样本的签名宽度问题，不证明下载成功，也不能外推所有认证都已失效。
- `S005/S015/S039/S086` 的 metadata 是上述公开 mode-2 形状。PanSou 附带的无关密码使 checkAccessCode 返回 `res_code:0, success:false`、无 shareId；不是保护分享的密码成功。
- 修复后其中两个目录正常返回树，两个单文件进入下载阶段后受控 401；不再因无关提取码误报密码失败。另加 `S204` 更换无关密码仍得到相同目录树。

## PanSou 与真实生产逻辑回归

仅调用既有 PanSou 搜索，不写 STRM/媒体，不调用 Emby。搜索关键词：流浪地球、庆余年、柯南、无损、电子书、Python、纪录片、音乐合集、英语。总计 **4029 条结果项（全局去重前）**，不是 4029 个已测或唯一分享。原七类散列分散抽样有 93 个候选、120 次 metadata/密码/root-page 请求；另有首轮与协议诊断，各自回执保留，不能混作完整 walk 成功。

最终使用链接相同生产 crate 的隔离探针，调用真实 `cloud189_service::parse`，目录成功后以 fresh client 调用 `fast_download`。共 **40 个不同分享、44 个样例（含 4 个密码变体）**，11 个批次均完整执行且保护检查通过：

| parse JSON code | 样例数 | 含义 |
|---|---:|---|
| 200 | 20 | 19 个不同分享的完整目录树，含一次相同目录的无关密码变体 |
| 401 | 7 | 单文件下载认证被上游拒绝，均带安全 auth / relogin 标识 |
| 404 | 14 | 分享不可用 |
| 403 | 2 | 错误提取码负例；未知保护状态不跳过校验 |
| 400 | 1 | 省略受保护分享密码的现有 metadata 不足分支 |

- 最大完整树 `S091`：**432 个目录（含根）、3357 个唯一文件**，约 59.756 秒；没有重复文件 ID。只是单次实测，不是性能基准或参考清单逐项相等证明。
- 本轮最大同目录直接文件数 **50**；**没有真实 >200 同级项目分页证明**，3357 个文件分散在多级目录中，不能据此关闭分页门禁。
- 20 次目录后的 fresh-client fast 地址请求均为受控 **401**，加上 7 个单文件 parse 的认证拒绝，实际取得下载地址为 **0**。
- 没有 CDN 请求、HEAD/Range 或文件/视频正文请求。探针结果是生产 service JSON 语义，不冒充 44 次线上 HTTP endpoint 或完整浏览器验收。
- probe 固定使用并发 4 以控制诊断负载；没有写回服务设置。2026-10-15 最终只读审计实际保存并发为 **16**，且全库与本次切换前内容相同；不是通过代码默认值猜出 16，也不是本次设置写入。2026-10-14 保存 4 的回执仍为历史观察，不覆盖新观察。

## 本地验证与部署保护

- 288 项 Rust 测试（Rust 1.94.1）、fmt、Homebrew Rust/Clippy **1.93.1** 的严格 all-target `-D warnings`、3086 前端 synthetic checks、隔离页面/权限/QR/HEAD/OPTIONS 验证通过。
- 额外工具链核对发现 **Clippy 1.94.1 严格检查失败**：未在本轮修改的 `src/http/mod.rs:75` 闭包触发 `clippy::result_large_err`。没有改动无关运行时代码或添加 allow 来压掉诊断，也不将 1.93 通过冒充 1.94 通过；`local-verification.json` 明确记录两个工具链及 `all_toolchain_checks_passed:false`。该遗留 lint 不等于编译/288 项测试失败，但工具链全绿尚未完成。
- guarded deployer 48 项测试、探针 runner 32 项离线测试和探针 1 项 tree-summary 单元测试通过；不是供应商成功下载证明。
- 最小五文件构建上下文，已有离线 musl 工具链构建；没有安装/清理工具链或 Docker 资源。
- 制品：binary SHA-256 `3d1cee097916854b2dca77dddc2ebacff2c1996c60f6095d2b797c9fad5004ff`，8631368 字节；source SHA-256 `e1db2d8dae1f8f0f4ceaff7c176251707d8ef23861f0ccbc7f5c461ab36ab9c0`；context SHA-256 `7419cfba3e9b13efead44f8b77041c89212755a507dd777c02fb63c8728fd591`。
- 新容器按不可变 image ID `sha256:987a058ed7cd89f54d11f9bdaa9180cc3eb980c351e1117739e773ef97565d1e` 启动，不按可变 tag 启动；实际运行 binary 字节已验证。
- 正常停止旧测试容器后 SQLite backup / quick_check、切换全部表内容相等；最终审计仍全部表内容相等，total/success/failed/cached 和 history 增量均 0，retention、session、网盘授权、env/seed 保留。没有 reset、扣减统计或恢复旧 DB。
- `jxpan-rust-test-prev-e897dd1e919a` 停止且 restart=no 留存；回滚只恢复代码，不回写 DB，所有测试回滚容器共用 DB，不得同时运行。
- 探针为独立非 root/read-only 容器，cap-drop ALL、no-new-privileges、64 PID、256 MiB、2 CPU；仅只读挂载公开 probe binary，按不可变服务镜像启动，不挂 DB/私有文件、不启动服务，凭据只在目标机读取并经 stdin 注入。
- 首次 probe runner guard 因 Docker 自动创建 bind mount 目标目录拒绝，第二次上传的保护判定又受 Docker inspect 的旧服务 Mounts 随机顺序影响。均先只读诊断，分别只放行已验证挂载的四项新增 scaffold、按完整 mount 对象排序比较；没有移除 rootfs/挂载/二进制校验，没有重传覆盖已上传制品，原失败回执保留。随后全部 11 个真实批次与最终审计通过。

## 证据与剩余工作

脱敏证据位于 ignored `.scratch/cloud189-fix-8789/`：

- `release.json`、`apply-result.json`、`verify-result.json`
- `protocol-paired.json`、`protocol-public.json`
- `regression-*-run-result.json`、`final-audit.json`、`final-report.json`
- `local-verification.json`、`source-manifest.json`

原始搜索、私有分享/密码仅存在 0600 private 文件，凭据不从目标机输出，均不提交。没有 stage/commit/push。

该历史窗口的后续建议为重新登录后补下载地址正例；后续 S005/S006 已有 302，不再把该阻塞描述成当前完全未取址。当前仍需在明确授权范围内补公开/保护、单文件/目录文件及普通/fast 的完整下载矩阵。真实 >200 同级分页、其它 provider 认证矩阵、真人扫码/长期续期、CDN/播放、长期尾延迟及线上回滚演练仍有独立门禁；8788 正式迁移、独立 final audit 与精确临时文件清理已核验，Git 结果以独立历史/回执为准。
