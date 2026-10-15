# 架构与职责

## 请求路径

```text
main                         进程启动、监听、维护任务与正常关闭
  └─ http::AppState           组装共享依赖，管理账号版本与缓存失效
      └─ http::router        路由与中间件接线
          ├─ admission       请求准入、总超时、响应体期间的许可持有
          ├─ request/params  有界请求解码与参数兼容
          ├─ home/admin      页面入口、权限检查与动作分派
          ├─ actions         记录、统计及设置动作
          ├─ shortlinks      短链创建与解析入口
          └─ parse           解析期限、认证恢复、结果与记录收尾
              ├─ provider_cloud   天翼请求适配
              └─ provider_mobile  移动请求适配
                  └─ providers    用例编排 → 网络客户端 → 协议与兼容内核
```

`http::security` 统一同源和 cookie 策略；`http::presentation` 负责页面、资源和公共响应头；`http::reply` 表示解析回复及缓存占用。网页和本地二维码资源保存在 `web/`，由路由模块嵌入。

## 核心模块

| 模块 | 单一职责与边界 |
| --- | --- |
| `auth.rs` | 管理账号生命周期、扫码会话、续期、租约和持久化后的状态发布 |
| `auth/credentials.rs` | 凭据解码、格式校验、启动来源优先级和快照投影；不负责网络或存储提交 |
| `providers/*_service.rs` | 组织各网盘解析用例，不承担 HTTP 路由或页面呈现 |
| `providers/*_client.rs` | 受限的供应商网络 I/O、响应校验及安全错误分类 |
| `providers/cloud189_client/{json,cookies}.rs` | 天翼客户端的长整数 JSON 兼容和响应 cookie 处理；不与授权内核的不同 cookie 规则混用 |
| `providers/cloud189.rs`、`providers/mcloud.rs` 与 `*_auth.rs` | 供应商协议、签名及兼容规则；已有网络/时钟抽象用于隔离测试 |
| `storage.rs` | 通过有界队列和独立 SQLite actor 串行化数据库操作 |
| `records.rs`、`shortlinks.rs`、`admin.rs` | 在同一存储边界内分别管理解析记录、短链和后台会话 |
| `cache.rs` | 有界 TTL 缓存、加载合并、内存预算和失效状态机 |
| `settings.rs` | 解析设置持久化与共享上游并发许可 |
| `compat.rs`、`tree.rs` | 共用兼容语义与目录树转换 |

## 设计约束

- 保留现有公共模块、类型和函数路径；内部拆分使用私有子模块，必要时在原路径重导出。
- 优先使用现有组合根、适配器、存储 actor 和测试注入点，不为形式上的 SOLID 增加无用途的 trait、动态分派、通用 provider 层或包装调用。
- 天翼和移动保持各自的业务及兼容规则；相似代码不代表语义相同。
- 账号先持久化再发布；不改变 CAS、租约、锁作用域、续期冷却、退避和最多一次的认证恢复重试。
- 不改变请求/上游并发、超时边界、缓存键与容量、数据库结构、SQL、连接池或下载流量策略。
- HTTP 只适配输入和呈现输出，不绕过各组件的授权、存储或供应商安全边界。天翼转存相关组件不接入公共解析路径。

## 验证

```sh
cargo fmt --check
cargo clippy --offline --locked --all-targets -j 2 -- -D warnings
env -u JXPAN_LIVE_ORACLE cargo test --offline --locked -j 2
cargo build --release --offline --locked -j 2
node tools/verify-frontend.mjs
python3 -B tools/test-verification-safety.py
```

`--offline` 需要本机已有锁定依赖；前端检查需要先安装 `tools/package-lock.json` 中的依赖。Rust 测试默认使用合成数据、内存网络或 loopback。

附加页面检查还需要本机已获授权、版本匹配的私有冻结基线（`.scratch/baseline` 中的 manifest、原始源码及 HTML）；仅有公开 checkout 无法运行该项。此检查只读冻结文件，运行时使用临时数据库及合成管理员，不加载真实网盘账号：

```sh
cargo build --offline --locked -j 2
python3 -B tools/verify-pages.py
```

保留直接编译供应商源文件的集成测试，以覆盖它们的 `cfg(test)` 路径。验证不得默认访问真实供应商、现网数据库或 CDN；在线验证需要另行明确授权。离线测试和源码等价审查不等于编译产物、性能或真实供应商行为的绝对相同。
