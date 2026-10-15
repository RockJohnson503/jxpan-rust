# 天翼公共下载：禁止 owned 回退的安全差异

## 边界与风险

公共 `parse` / 指定文件解析 / `parse_cloud189_file` / fast 下载必须只解析分享文件。caller 提供的 `share_id`、`file_id` 或既有 sid **不代表已获授权访问服务端账号私有文件**。

冻结部署的普通下载顺序为 Open → App dt=1 → App dt=3，fast 为 App dt=1 → App dt=3 → Open。最后的 App owned 请求是 `getFileDownloadUrl.action?…&fileId=…&dt=3&flag=1`，**没有 shareId**，使用服务端账号会话。在分享方法没有返回 URL 时继续请求 owned，可能把任意 fileId 解释为服务端账号自己的文件。只禁用转存不足以关闭这个边界：owned 直链请求本身不需要先转存。

当前 `cloud189_service::DownloadMethod` **已移除 `AppOwned`**，公共 service 的生产 adapter 与 callback 下载选择严格只有 `DownloadMethod::{Open, AppShared}`，不是仍待实现的策略建议：

| 入口 | 允许顺序 | 两种方法耗尽后 |
| --- | --- | --- |
| 普通单文件 `parse` / 目录中指定文件 / `parse_indexed` 的文件分支 | Open → AppShared (`dt=1` + shareId) | code=500、success=false、data=null |
| 文件动作 `parse_cloud189_file` / `parse_file` | Open → AppShared (`dt=1` + shareId) | code=500、success=false、downloadUrl=null，保留 camelCase 文件元数据 |
| fast / `fast_download` | AppShared (`dt=1` + shareId) → Open | `Err` 中 code=502、success=false、data=null、msg=`无法获取直链` |

所有尝试保留分享上下文；遇到首个非空合法 URL 立即停止。缺失 URL、空字符串及请求异常只进入下一个**公共分享方法**，不得切换到 owned，也不得自动转存。普通失败文案为 `获取下载链接失败（只读模式未启用转存回退）`，文件动作为 `下载链接获取失败（只读模式未启用转存回退）`。这些静态文案保留既有只读协议，不输出上游错误或凭据。

HTTP 已在 `src/http/parse.rs` 接入上述公共 service：普通分享解析、`/?action=parse_cloud189_file` 和 `type=down&id=…` 都不回调到 owned/transfer 聚合 API。表中的普通/file action code 是 JSON 业务码，当前普通 JSON 包络仍使用 HTTP 200；fast 两方法耗尽则由 `Reply::fast_error` 保持 HTTP 502 与 JSON code=502 一致，成功才返回 302。失败不作为 ready-cache 保留；共享状态、下载缓存、调用者 sid/fileId 或后台已配置账号均不是私有文件授权。

`cloud189_client` 的 owned primitive 和底层聚合 API 仍存在，但不是公共 route 的回退入口。独立 `cloud189_transfer_client` 与 TransferKernel 也已导出、由 root tests 覆盖，却默认关闭且未接入公共解析。未来只有显式授权、可信同账户绑定及受控 scope 的内部 transfer 流程才可调用 owned；OAuth 凭据与 QR SessionKey 的同账户绑定目前未实现/未验证，不能从分享查询参数、缓存或共用 AppState 推导出来。

这项变更**不声称**已解决全部 route 鉴权、分享归属验证、认证失败恢复、token pool、目录下载或 transfer 闭环问题。已知目录/缺参 guard 继续保留；[folder_item 的冻结 missing-idMap 断链](cloud189-actions-index.md#冻结部署的-folder_item-断链不可伪称已修复)也不在此修复。**service 仅返回 URL 数据，不请求 Range hop 或下载正文**；HTTP fast 包装层另有受限的 `Range: bytes=0-0` 请求（仅无凭据 HTTPS 189.cn 或其子域、禁止自动跳转），它不新增 owned 请求或转存授权，不能把两层混称为完全没有下载地址探测。

## 冻结部署与当前实现：不能伪称完全相等

原 worker SHA 固定为：

```text
47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118
```

原始执行证据保持不变：

- `tests/fixtures/cloud189-service.json`：29 个 service 向量。
- `tests/fixtures/cloud189-action.json`：20 个 file/folder action 向量。
- `tools/cloud189-service-vectors.mjs`：提取冻结 class、shared walker、fast 原函数，在 networkless VM 中执行。
- `tools/cloud189-action-vectors.mjs`：Acorn 提取冻结 action IfStatement，并执行原 class/walker；合成 D1 credentials、固定时间，任何非预期 fetch/转存立即失败。

**不修改 frozen expected、fixture 或 VM 逻辑以迎合安全实现。** `JXPAN_LIVE_ORACLE=1` 是重新运行上述离线原方法，不是连接生产或真实云盘；`tests/support/mod.rs` 仍先核验 VM 输出与 frozen fixture 全量相等。

Rust 差分测试分别断言历史结果与新结果：

| 冻结向量 | 冻结结果 | 当前公共结果 | 当前调用 trace（省略 share 准备） |
| --- | --- | --- | --- |
| service `single-app-dt3` | 200，URL 仅由 owned 提供 | 明确 500 / success=false / data=null | Open → AppShared |
| service `fast-app-dt3` | 302，owned URL，未尝试 Open | 明确 502 / `无法获取直链`；Open 无 URL 不能保留历史成功 | AppShared → Open |
| action `app-owned` | 200，owned URL | 明确 500 / success=false / downloadUrl=null | Open → AppShared |
| service `ordinary-no-direct` | 500，原 token/login 聚合失败信息 | 原有只读、安全静态 500 | Open → AppShared |
| action `no-direct-no-transfer` | 500 / `下载链接获取失败` | 500 / 显式只读文案、downloadUrl=null | Open → AppShared |
| service `fast-open-fallback` | AppShared → owned → Open 后成功 | **保留 Open 成功**，删除中间 owned 尝试 | AppShared → Open |

其他不涉及安全/既有只读差异的响应继续直接比较原 expected。action 测试不再把当前失败文案归一化成旧文案后宣称响应相等。

### trace 不能只过滤 dt=3

`expected_public_calls` 从原语料保留准备/列举调用，针对确实进入下载的业务分支重新计算两种公共方法顺序，并按该向量的公共下载 URL 成功早停。它不参考当前实现的实际 trace 来生成期望。

这比简单删除原 trace 的 owned 调用更严格：原 `fast-app-dt3` 因 owned 成功而早停，原 trace 没有 Open；当前在 AppShared 失败后仍应执行 Open，若 Open 也无 URL 就必须失败。相反，有合法 Open 或 AppShared URL 时必须成功，不能为了安全差异一概拒绝。

实际 loopback fast 测试覆盖 `fast-app-dt1`、`fast-app-dt3`、`fast-open-fallback`：

- stub 仅按当前公共尝试分支提供 1 或 2 个预期回复，不等待已删除的 owned 调用。
- 排除原 Range hop 和 `dt=3` 请求；按当前实际应执行的公共方法比较路径、query、签名 header。
- 原 owned 成功分支缺少 Open transcript 时，复用同一冻结 corpus 的 `fast-open-fallback` 中原 Open 请求。合成 fileId/shareId、凭据、时间均相同，不改写原 trace，不伪造历史 Open 成功。
- 断言 `dt=1`、shareId/fileId 保留、没有 owned `flag=1`、没有 Range 请求。

## 专门离线回归

`tests/cloud189_service.rs` 新增两项回归：

1. `arbitrary_public_ids_never_fall_back_to_owned_callbacks`
   - 两组任意合成 shareId/fileId：假名称和假数字 ID。
   - 普通单文件 parse、目录目标文件 parse、file action、fast 四个分支。
   - 公共方法返回 `None`、空 URL、异常时逐一断言明确失败，只尝试两种分享方法，异常内容不泄露。
   - 单独验证合法 Open / AppShared URL 仍成功，保留文件名编码，且第一方法成功时早停。
   - 共 2 × 5 × 4 = **40 个业务场景**，不请求网络。
2. `arbitrary_public_ids_actual_http_exhaust_shared_methods_without_owned`
   - 同样两组 ID、四个分支，以实际生产 service + adapter 向 `127.0.0.1:0` 发 HTTP。
   - 两方法都返回显式空 URL 时断言失败；Open 提供 URL 时断言保留成功。
   - 共 2 × 4 × 2 = **16 个 loopback 场景**；逐请求核对允许的路径、顺序、dt、shareId/fileId，拒绝 owned flag 和 Range。

loopback client 仅使用 crate 测试专用的 loopback 构造，禁止代理和自动 redirect，使用合成凭据与固定时间，不提供生产 endpoint override。stub 在精确回复脚本结束后仅保留 100ms canary 窗口：意外第三次请求会被记录并回复 `https://offline.invalid/forbidden-owned` 诱饵 URL。无论错误被吞掉还是 owned URL 被错误接受，调用次数/参数/响应断言都会失败。canary 不等待第三次请求；accept deadline、read timeout 和 client timeout 均有限，不会因为移除 dt=3 而 hang。

原 root 索引碰撞、空目录/未投影 ID、真实 root、单次 walk、400 节点/6 并发以及生产 future `Send` 测试保留。

## 离线核验

在仓库根目录运行：

```sh
# 原 action VM 与 frozen fixture 完全相等。
node tools/cloud189-action-vectors.mjs --check-fixture --summary

# 两个原 VM 重跑并由 tests/support/mod.rs 核对 frozen fixture；
# Rust 同时验证明确的安全差异、两方法 HTTP 和原索引/Send 回归。
JXPAN_LIVE_ORACLE=1 cargo test --offline --test cloud189_service
```

公共 enum 与 adapter 的 owned 分支已删除；`cloud189_service` 回归已通过 **11 项**离线 synthetic / loopback 测试，包括任意 shareId/fileId 只耗尽两种分享方法、fast 保留 Open 成功、无第三次 owned 请求，以及 root 索引和生产 future `Send`。原 action/service oracle 的 20 + 29 个 frozen 向量保持不变，必要安全差异单独断言。本次授权现网只读复验：天翼目录 JSON 相等，旧/Rust 下载均 502，真实成功下载要求仍失败，live verifier 保持退出 1。全套本地 175 项测试通过也不代表部署上线、真人 QR、成功天翼下载/转存或性能闭环；真实云盘写操作仍需独立显式授权。
