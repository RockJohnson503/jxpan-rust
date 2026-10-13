# 天翼只读编排（部分兼容，不是完整闭环）

基准部署 `_worker.js` SHA-256：
`47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118`。
使用 `.scratch/baseline/Cloud189Parser.js` 原类、`readable.js` 原共享 walker / nested builder / fast 函数；不读取 `credentials.json`，不联网。

## 接口与接入约定

`src/providers/cloud189_service.rs`：

```rust
pub async fn parse(
    client: Cloud189Client, share_url: &str, pwd: &str,
    fid_chain: &[String], file_id: &str,
) -> serde_json::Value;

pub async fn fast_download(
    client: Cloud189Client, share_url: &str, pwd: &str,
    file_id: &str, sid: &str,
) -> Result<String, serde_json::Value>;
```

- 主 agent 负责在 `providers/mod.rs` 导出模块、整合路由、enabled / 凭据门槛和真实只读核验；此任务没有修改模块清单或主路由。
- 输入合法性/域名授权由 caller 负责。复用 `cloud189::extract_share_info` 的部署语义（含宽松匹配），不能把它当域名安全边界。
- 构建 reqwest Client 时禁用自动重定向；不能启用全局或跨请求共享 cookie jar / 默认秘密请求头。服务只访问 HTTP 内核的固定官方 API，返回直链是数据，不 fetch 下载内容。
- 服务消费传入 client；share info / password 在同一实例顺序完成后，才为每个目录克隆实例。捕获的 cookie 和 credential 字符串随 clone 复制，reqwest 连接池可复用，不持有 mutex 跨 await。
- 仅兼容地新增 `Cloud189Client: Clone`，暴露 `download_open` / `download_app`；缺少对应签名凭据时返回 `Ok(None)`，不发空凭据请求。已有 `download_url` 顺序和 HTTP 合约不变。

## 已实现

### 普通解析

1. 分享提取 → share info → 有非空白密码时验证；验证成功的 shareId 覆盖 share info。
2. `isFolder` 为真或 fileSize 为 JS falsy 时按目录处理（零字节文件也被视为目录，这是部署行为）。目录没有 sid 时返回原错误。
3. `crate::tree::walk` 执行 6 并发 / 400 admitted nodes 的全树 BFS，仅一次遍历；共享 walker 原有的去重、错误节点、名称冲突、顺序等规则不重新实现。
4. `fid_chain` 取最后非空末项；基于共享 `nested_tree` 输出 `{ tree, sid }`，`shareKey = c189:<sid>`。目录 ID 不存在时保留部署 fallback node / requested nested ID 的怪异行为。
5. `file_id` 优先于 fid projection，在同一 Walk 的 `all_files` 定位；目录 ID（包含 root）拒绝直接下载，缺失文件返回安全静态 404。
6. 文件直链顺序为 Open → App `dt=1+shareId` → App `dt=3&flag=1`。输出部署 `parse` 的 `file_name/file_size/download_url/is_folder/file_count/files` 数据形状。**不调用转存**。

### fast download

- 已有 sid：完全跳过 share URL 提取、share info、pwd 检验（即使 URL 格式无效或 pwd 错误）。
- 没有 sid：读取 share info；pwd 只要非空就校验（空白密码也校验），只消费返回 shareId，不因 `success:false` 丢弃原 sid，保留原 fast 语义。share info 为 null 时仍允许密码检验得到 sid。
- root 对应目录/零字节文件拒绝；返回 `download.cloud.189.cn/file/downloadFile.action` 的直链也拒绝。
- **有 sid 时无法仅凭 file ID 判断目录**。caller 必须先用自己的已加载共享树/cache 判断 `tree.nodes[file_id]`，并拒绝目录。这是保留 sid 快路径且不额外 list/share 的必要前置条件；不能声称服务本身已覆盖所有目录判断。
- App `dt=1` → App `dt=3` → Open；失败返回 502 / 静态错误数据，目录返回 400 `目录无法下载`；成功只返回 URL 给 main。

## 未实现 / 有意差异

| 范围 | 当前行为与边界 |
| --- | --- |
| 普通文件转存 fallback | **缺失**。部署 Open 失败后可能转存再 App；这里只读 Open/App。全部失败返回 `获取下载链接失败（只读模式未启用转存回退）`，不会误称完整兼容。 |
| token pool / 续期 / login / cookie 主动补全 / 动态获取 Open token | **未实现**。一个 caller 选定的 credential snapshot；不从 D1/KV 读身份，不扫码、刷新或续期。 |
| transfer / delete | **未实现且禁止调用**；没有相关 HTTP 入口。 |
| 跨请求目录/play cache | **未实现**，main 可复用共享 cache。单次解析只生成一次 Walk，sid 快路径的 cached directory rejection 由 main 负责。 |
| fast Range hop | **有意不执行**。部署会对直链 GET `Range: bytes=0-0`, `redirect: manual`，可能使用 Location。VM 运行原始 hop 并记录 trace；Rust 服务没有该请求。若需要 hop 后 URL，main 另做受控策略；不能声称当前返回 URL 已通过跳转实测。 |
| fast 302 Response / HTTP headers / play-cache | **不在服务层实现**；main 将成功 URL 转为 302 或其它响应。 |
| targeted file_id | 共享 Walk 定位与目录拒绝是安全扩展。部署 `resolveItemInFolder` 使用旧 idMap / camelCase file 数据，而新的 browse 已返回 nested tree，旧 action 的组合不保证成功；这里只返回普通 `parse` 的 snake_case 文件数据，不宣称原 action 完全兼容。 |
| 异常消息 / 全部认证失败 | HTTP/回调异常用静态安全消息，不包含异常原文、URL、密码、token、cookie。普通全部失败不用原 tokenpool/need_relogin 诊断，避免推断 session 过期。 |
| 对应下载凭据缺失 | 跳过该 API；不会发送空 Open token / App secret。callback 测试 seam 无凭据概念，生产内核负责实际 gate。 |
| 共享树安全差异 | 继承 `crate::tree` 的循环/深度保护、`__proto__` 安全数据 key 等，见共享树文档；不是另造 walker。 |
| pagination | 沿用现有 HTTP 内核 list 的 page 1 / pageSize 200，未增加分页；全树不等于每目录无限文件完整分页。 |
| 空/失败目录节点 | 保留部署 walker 空预加载节点语义，可返回成功空 tree；不是成功访问每个目录的证明。 |

## 当前部署差分验证

```sh
node tools/cloud189-service-vectors.mjs --summary
cargo test --offline --test cloud189_service --test cloud189_http
```

需要本地 Node + 项目现有 acorn；源部署必须在 `.scratch/baseline/`。测试不采用旧 fixture fallback，缺少基准或 SHA 不一致会显式失败。生成器校验原类包含于 readable，AST 提取原函数并输出函数 hash。

`tests/cloud189_service.rs` 包含：

- 29 个 VM 场景：root/deep tree、missing fid fallback、空/失败子目录、重复目录/名称冲突、6 并发/400 节点、普通 Open/App dt1/dt3、0 size、密码覆盖/失败/空白、无效分享/空 info/无 ID/无 sid、普通 no-direct 的显式差异、fast App/Open 顺序、已有 sid 跳过 share/pwd、root/0 size/下载目录 endpoint 拒绝、fast pwd 失败仍沿用 sid、无 sid/无直链。
- VM 普通 root 使用**原类 parse**；deep projection 使用**原类 browse + 原共享函数**的明确方法级组合，不把它标成完整主路由差分。禁止 transfer，HTTP strictfetchstub 只允许原下载 API 和预声明合成直链 Range hop；捕获任何额外 fetch 后测试失败，不会联网。
- 逐项比较普通 JSON / fast 成功 Location 或失败 JSON、share/password/list/download 方法顺序、walker peak、400 节点上限。普通 no-direct 的原 tokenpool 错误与只读错误单独标注，不强行算等价。
- 3 个 fast 场景另以 Rust **实际生产 adapter** 连 loopback stub，比较原 fast 函数的原始 App/Open HTTP trace（URL path/query、方法、所有部署请求头包括签名）。仅去除 HTTP framing/header defaults；明确排除原 Range hop（服务边界差异）。
- 真实生产 parse 的 loopback 测试验证 password 捕获 cookie 传播到 root + 6 子目录克隆，预捕获独立 clone 不得到 cookie；无全局 cookie jar。
- targeted file ID 文件定位 / root 和深层目录拒绝 / 缺失 ID、回调错误不泄密、无 share info 仍可密码取得 sid 为独立扩展/安全测试。

**当前结果只证明上述合成只读范围与标记差异；未读取真实凭据，也没有线上核验。完整部署闭环仍未完成。**
