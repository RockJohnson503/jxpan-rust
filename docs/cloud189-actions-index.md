# 天翼动作与原始目录索引边界

本文描述本轮工作区的只读纠错，**尚未部署或通过真实大分页/天翼成功下载验收**。冻结 action/fixture 不改写；旧 bug 不再作为当前生产行为照抄。整体状态见 [STATUS](STATUS.md)。

## 原始目录索引与完整 Walk

两 provider 的 `parse_indexed(...) -> (Value, Option<(String, Vec<String>)>)` 参数同原 parse；parse 取 `.0`。生产共享树使用 `tree::walk_complete`，索引来自**同次完整 Walk**，无第二次网络遍历、不从展示名称树反推。

- `Some((sid, ids))` 的 ids 包含真实 walk root 和 all_folders 的原始 fileId，保留空目录、重名目录以及投影子树之外的目录。移动 root 为 `root`；未运行 Walk 的移动根单文件路线返回 None。
- 当前全局 ID 必须唯一；重复文件/目录 ID、跨父目录复用或循环拒绝，不再说“重复 ID 保留并由 HTTP cache 去重”。失败/超限不发布部分成功索引。
- 类型使用生产显式规则，`"0"`/`"false"` 不按旧 JS truthiness 变目录。索引独立于重名投影，`nested_tree_complete` 保证重名不覆盖。
- HTTP 调用方负责有界目录缓存、share/password/account/revision 隔离和下载 guard；service 不保存 cache。冷缓存 fast 仍不能宣称已识别所有目录。
- observer 同步复制 ID，不持 std mutex 跨网络 await；future 的 Send 由测试断言。

后台 concurrency 默认 4、范围 1–16；walk 取本轮快照并滚动补位，底层两网盘分享 send + 完整 body 使用实时共享预算。auth 刷新/QR 未纳入；设置的持久化/取消语义见 [PARSE_SETTINGS](PARSE_SETTINGS.md)。

完整 walk 任一上限先触发即错误：**10k 目录（含根）、20k entries、8 MiB 保留估算、96 层**。每目录 **200/页、100 页、累计 4 MiB 估算 / 20k items、每响应 2 MiB**。这些不是 RSS 上限或真实大分页证明。

## 天翼公共动作

```text
browse_folder_item(client, share_id, folder_id, target_file_id, pwd, share_mode, share_code) -> Value
parse_file(client, share_id, file_id, file_name, file_size: &str, pwd, share_code) -> Value
```

### folder-item：修复旧 missing-idMap 断链并限定子树

- `folder_id` 为空时，walk 根为 target_file_id；非空时，根为 folder_id。
- 完整 Walk 后在该范围内查目标。目标目录返回它的名称树和 sid；目标文件使用该 Walk 的真实 metadata 并获取公开下载地址；目标不存在返回 404。
- 目标在分享其它分支也不能越过指定 folder 的范围命中；不是改成“全分享搜索”来绕过原断链。
- 原始 ID/metadata 决定定位，不伪造旧私有 `_shareInfo/idMap/_folderTreeCache`，也不从名称投影猜 ID。
- 上游失败、重复 ID、循环和超限明确失败，不把错误目录当空节点返回成功。合法空目录仍可成功。

### parse_file：以供应商 metadata 为准

HTTP 兼容签名仍接受 caller 的 file_name/file_size，但生产不信这些值：使用分享元数据/完整目录列表定位文件，再返回上游 fileName/fileSize/fileId。目标为目录 400、不存在 404；不是拿任意 caller ID 请求下载后伪造文件描述。

文件动作仍是 legacy camelCase `{type,fileName,fileSize,fileId,downloadUrl}`，不同于普通 parse 的 snake_case 单文件结构。大小未知 **null**，真实 **0** 有效；不经 JS parseInt/浮点舍入伪造大小，大于 JS 安全整数使用字符串。合法十进制上游字符串保留字符串。

**不再追加 filename query。** 下载 URL 原样返回，避免额外拼接破坏签名、fragment 或其它供应商语义；旧 encodeURIComponent/parseInt 向量保留作历史依据，不代表当前 metadata 规则。

### 提取码与 metadata 重取

具有 share_code 与非空 trimmed pwd 时校验提取码，失败明确 403，不仅记录日志后继续。成功捕获分享 cookie，并采用响应有效 shareId；初次分享 metadata 被密码保护隐藏时带 cookie 重取。不会为“与旧 action 相等”而丢失密码授权或强用错误 caller shareId。

## 只读下载边界

- 普通 parse / 文件动作：**Open → AppShared（dt=1 + shareId）**；fast：**AppShared → Open**。成功立即早停。
- **禁止 App dt=3 无 shareId 的 owned 回退**，防止公共 caller ID 读取服务端账号私有文件。owned 底层 API 仅留给独立、需授权的内部能力，不在公共入口接线。
- **不转存、不删除**。冻结转存参数倒置、固定账号根 `-11` 及可能删除已有文件仍是历史安全证据，不因本次纠错启用 transfer。
- 合法公开方法耗尽仍明确失败：普通 parse 500/success=false/data=null，文件动作 500/success=false/downloadUrl=null，fast 502；不靠 owned/转存/普通解析 fallback 兜底。
- Range 一跳探测已移除；HTTP 直接供应商 URL **302**，服务端不跟随 CDN、不拉文件/视频正文。302 不是天翼成功下载或 CDN 播放验收。

公共下载的早期安全差异依据见 [CLOUD189_PUBLIC_DOWNLOAD_SECURITY](CLOUD189_PUBLIC_DOWNLOAD_SECURITY.md)；该文中的历史测试数不能代替本轮完整性修复结果。

## 冻结缺陷证据与离线核验

冻结 `handleMainRequest.js:1541-1580` 将 `browseFolderInteractive.data` 传入 `resolveItemInFolder`，但 `Cloud189Parser.js:1689-1692` 只给 tree/sid，没有 idMap/_shareInfo；resolver 在 `:1695-1703` 因此返回 404。**这是冻结旧版缺陷，当前已由上面的原始 Walk 定位纠正，不再列为待决定是否修复。**

冻结 file action/resolve 反向传参 `(fileId,shareId,fileName)` 给 `saveAndDownload(shareId,fileId,fileName)`，且该方法可写根目录/删除文件。公共路径继续不调用它。

`tools/cloud189-action-vectors.mjs` 校验 worker SHA `47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118`，networkless VM 执行精确原 action/class/walker。原 **20 action / 29 service 向量**保留，是冻结兼容与已知缺陷证据；owned 成功、旧 missing-idMap、caller 大小/filename 等有意不同之处必须独立断言，不重写 frozen expected 或忽略差异伪称完全等价。

```sh
cargo test --locked --offline --test cloud189_service --test mcloud_service --test tree_complete -j 2
cargo test --locked --offline --test cloud189_http --test common_http -j 2
# 仅重跑本地冻结 Node oracle，无供应商网络：
JXPAN_LIVE_ORACLE=1 cargo test --locked --offline --test cloud189_service --test mcloud_service -j 2
```

合成/loopback 回归验证子树文件/目录定位、metadata、密码 cookie、零/未知/大大小、重名、失败拒绝、公共请求顺序与早停。实际 adapter 应只出现 dt=1 且保留 shareId/fileId，不出现 owned flag 或 Range 文件请求。这里列验证路径，不宣布本轮全套通过或部署成功；最终重测与实机结果另见 STATUS 追加记录。真实大分页、天翼成功下载、扫码/续期和完整场景矩阵仍待验收。
