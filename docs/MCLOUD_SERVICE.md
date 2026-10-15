# 移动云盘只读业务编排、完整性与差分

## 范围与接入

`src/providers/mcloud_service.rs` 已由 provider 模块导出并接入 HTTP，复用 `MobileCloudClient` 和共享 `tree::{walk_complete,nested_tree_complete,Walk}`。业务层只列目录、请求公开分享下载地址；不登录、续期、转存、删除或保存凭据。授权、缓存、历史和 HTTP 302 由外层接线；返回直链不 fetch 下载正文。当前状态见 [STATUS](STATUS.md)。

主要入口：

- `parse(client, config, share_url, pwd, fid_chain, file_id) -> Value`。
- `parse_indexed(...) -> (Value, Option<(String, Vec<String>)>)`，参数同 parse；一次 Walk 同时给出结果及真实目录索引。
- `parse_with_fid_chain(client, link_id, pwd, fid_chain, file_id) -> Value`，专门子目录 action 的最后目录列表，不是完整树入口。
- `ServiceConfig { enabled, authorization_configured }` 从同一授权快照取得；禁用优先 503，未配置 401，再检查链接。配置非空不证明上游认证有效。

`parse_mcloud_subfolder` 的 file_id 分支由路由转到普通 indexed 定位；无目标文件的目录 action 走 fid-chain 列表。client 必须固定供应商端点、正常 TLS 验证、禁止任意重定向，不把输入分享 URL 当作 HTTP endpoint。测试仅有 cfg(test) loopback seam。

## 当前生产行为

| 路线 | 行为与完整性 |
| --- | --- |
| 根只有一个文件，且无 fid/file ID | 完整根列表成功后返回原首页 list 字段；download_url=null，不预取直链；不运行整树，目录索引为 None |
| 多项/包含目录或需要定位 | 从 root 运行 walk_complete；首次 root 列表直接复用，不再请求一次 root |
| 普通 fid | 完整 Walk 后仅投影最后 fid 的子树；缺失返回 404，不回退根目录 |
| 普通 file ID | 在同一个完整 Walk 的原始文件索引定位，不再深度 8 DFS/首次匹配；目标目录 400，不存在 404；失败的其它子目录也不能被忽略 |
| 定位文件后的下载 | 用该上游文件 ID 请求下载地址；失败受控 502，不返回 download_url=null 的假成功 |
| 专门 fid-chain 列表 | 使用最后 fid（无 fid 为 root）的完整分页列表，规范化并返回 folder/list、current/parent 字段；不是证明整条父子链或整个分享已完整遍历 |

生产列表使用 `list_files_checked`，不将 transport/解密/JSON/上游失败折叠成 `[]`。根/子目录读取失败是 502；树 metadata/限额错误有明确错误响应，没有部分成功 tree。`tree_complete:true` 只在该路线实际完整完成后输出，不从旧 `have_page:false` 推导。

### 大小、类型与重名

- 完整树及 list 的字节大小取上游非 null 字段，未知 **null**，真实 0 有效；非负整数字节数大于 `2^53-1` 用字符串，合法十进制字符串保留为字符串；非法 metadata 不能默默造 0。
- 目录类型不复用 JS truthiness，字符串 `"0"`/`"false"` 不是目录。
- `nested_tree_complete` 保留 `{id,size}` 文件叶子形状，但重名不覆盖：ID 后缀仍冲突就继续加序号。
- 文件/目录 ID 在全次 Walk 中唯一；重复、跨父目录重复、类型碰撞、根回环拒绝。目录索引来自真实 root 和 all_folders，不从名称树反推，完成之前不发布部分索引。

## 并发与硬边界

后台 `get_parse_settings` / `set_parse_settings`：整数 **1–16**，默认 **4**，`rust_parse_settings` 无 TTL；缺设置启动不写 KV。SQLite 已接收的更新在 HTTP 取消后仍写入并串行发布，写失败不发布。详见 [PARSE_SETTINGS](PARSE_SETTINGS.md)。

- 每次完整 walk 固定调度并发快照，`FuturesUnordered` 滚动补位，不等待慢兄弟目录组成整批。
- 天翼/移动公开分享 I/O 共用动态预算，从 send 前占用至完整 body 完成/错误/取消；调小不打断旧请求，调大唤醒等待者；auth 刷新和 QR 客户端未纳入。
- 完整 walk：**10,000 目录（含根）、20,000 entries、8 MiB 保留估算、96 层（根为 1）**，任一上限先触发即报错，不截断成功；字节估算不是 RSS 上限。
- 每个目录：**200/页、最多 100 页、累计 4 MiB 保留估算 / 20k items；每响应 2 MiB**。caLength/coLength 分别核对 ca/co total；不用 nodNum 作总数。opaque nextPageCursor 下一页原样传递，重复 cursor、重叠 ID/页、总量变化、不推进或超限报错。
- **真实 >200 项分享与 cursor 契约尚未实机验证**。离线请求字段断言不是供应商分页验收。

所有 futures 属于当前请求，取消/失败时释放；不 spawn 永久后台遍历，不持 std lock 跨网络 await。外层另有限制：HTTP 64 准入、参数/body 15 秒、handler 150 秒、parse 120 秒。

## 冻结差分与历史结果

最初 `tools/mcloud-service-vectors.mjs` 从部署基线提取精确原类及共享旧 walker，使用 networkless VM 与 synthetic list/download callback：

- worker SHA-256：`47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118`。
- class SHA-256：`4c2784a32492b016548fcbf6286a71fd2ca79b995280aeb3eff8d45bba680661`。
- 组合源码 SHA-256：`fd47bc886e26eb579830d32abc137cd4c1c90c6c67b5671c769c40649cda190b`。

原 125 案例/2 测试与“22 案例少 1 次 root 请求、最高 4 callback、400 节点”是**最初等价实验的历史结果**，不能套用到当前完整性路线。冻结工具/fixture 保持旧行为；现在允许纠正旧 bug，缺 fid、错误吞空、深度/截断、重复 ID、大小与 path/coPath 等差异需要独立断言，不能改 oracle 伪称逐字等价。

```sh
cargo test --locked --offline --test mcloud_service --test mcloud_http --test tree_complete -j 2
# 以下开关只重跑本地冻结 Node VM，不请求云盘：
JXPAN_LIVE_ORACLE=1 cargo test --locked --offline --test mcloud_service -j 2
```

Node oracle 需要现有本地 baseline 与 acorn；缺失时明确失败，不能跳过后声称通过。此处列复现命令，不宣称本轮全套测试已通过；主流程仍需完成最终重测/部署验收。

## 未完成与审查边界

授权/缓存/记录已在外层接线，不再标成“未导出/由主代理将来接入”。刷新 Err 不保留 ready cache，移动原有一小时失败退避保留；缓存 TTL/账号 revision 隔离见 [CACHE](CACHE.md)。

2026-10-14 两 MP4 的正确 metadata 与 302 是历史实机结果；该轮未跟随到 CDN。本轮完整性/分页尚未部署，不声称真实大分享、真人扫码/续期、完整场景矩阵、CDN 播放或同环境性能已通过。后台 session 未绑定当前配置版本、旧 DB 权限不强改，属于本轮保护线上状态的保留边界；不是将活动 session/已有数据静默重置后宣称审查完成。
