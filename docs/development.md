# 开发与构建

Rust 依赖公开 `gateway-plugin-sdk`，固定到宿主提交 `1a2a4a56aed0a9660f173cb1d125b9728e2f214c`；不依赖宿主 Core、Provider 或 Store。Rust 工具链为 1.99.1，前端使用 Vue 3、TypeScript、Vite 与固定发布包 `@codex-proxy/ui` v0.8.0

```bash
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked
pnpm --dir frontend install --frozen-lockfile
pnpm --dir frontend lint
pnpm --dir frontend build
```

前端使用 Node.js >=24，`frontend/package.json` 固定 pnpm 12.6.0

## 代码职责

| 入口 | 职责 |
| --- | --- |
| `identity.rs` | 读取明确会话身份并隔离认证后的 Key |
| `policy.rs` | 参数校验与结构化上游错误码识别 |
| `store.rs` | 封禁、待核实归属、拦截计数、历史、精确 CAS 和手动解除 |
| `ledger.rs` | 按请求、Key、账号及尝试核实原生记录中的真实错误码 |
| `native.rs` | 通过受管 HTTP 回调消费既有原生管理接口 |
| `app.rs` | 请求与尝试准入、原始正文直通、完成观察和停止重试 |
| `management.rs` | 管理路由、服务端分页与 UTC 展示文本 |
| `settings.rs` | 复用原生实例 revision、参数更新与发布 |
| `frontend/src/composables/useBlockHistory.ts` | 记录查询、解除状态和迟到结果隔离 |
| `frontend/src/components/` | 记录详情与按需加载的设置弹窗 |

request 与 attempt 中间件直接返回 `next` 的原始响应，不读取正文、不创建转发任务。同步错误保留原始快照，重试处理只停止已经确认的策略命中

完成观察只订阅 `request_completed`，正常完成和取消仅回收进程内关联；失败请求核对认证 Key、实际账号和尝试序号，再通过原生详情确认真实上游错误码。原生记录未落库时复用待核实记录，封禁从宿主给出的完成时间计算

请求关联只保存身份摘要与最新尝试归属，不作为封禁事实。首批准备失败或取消时立即回收，成功响应的关联由完成观察回收。为限制通知丢失后的遗留内存，最多缓存 1024 条，满时淘汰最早请求并报告容量错误，不阻断新请求；被淘汰请求的后续完成观察不会建立封禁

管理页通过 `window.codexProxyPlugin.request` 访问当前实例的注册路由，沿用宿主主题与尺寸同步。设置更新保留全部已有绑定，只修改当前实例参数，保存成功或结果未知后不再请求旧 revision

## 原生记录核实

`host.dispatch_http` 调用既有 `/api/admin/usage/records/detail?id=<requestId>`，使用宿主签发的插件身份，无需管理员密码。只接受精确请求与认证 Key 的归属，并核对账号、尝试序号和 OpenAI Provider；不解析消息文本，不保存原生详情正文

`blocks` 状态版本为 2，封禁、历史与待核实归属在同一分片内 CAS 更新；版本 1 升级保留历史并补齐空待处理列表。待核实归属冻结发生时间和时长，解除会同时消费当时已发生的待核实失败，迟到查询或完成通知不会重新封禁解除前已发生的失败

声明 `maintenance`，每轮最多核实 128 条；准入只读取当前会话所属分片，不等待当前逻辑请求自己的终态。维护、准入与管理查询允许并发，核实提交时精确复验待处理快照。全部能力在插件内实现

## 打包

准备与当前公开 SDK 合同一致的 `cpr-plugin` CLI 后执行：

```bash
CPR_PLUGIN_CLI=/path/to/cpr-plugin bash scripts/package.sh
```

默认目标为 Linux x86_64，产物在 `dist/`。`CPR_PLUGIN_TARGET` 可选择实际可构建的平台，不能仅修改目标名伪装交叉编译

脚本构建页面及 Rust 二进制、生成第三方许可，再调用 CLI 生成 `.tar.gz` 与 SHA-256 文件。安装只上传插件制品，不替换宿主文件

## 验证

`tests/` 验证会话隔离、精确错误匹配、到期边界，以及真实插件子进程与受控宿主之间的 RPC、状态 CAS、原错误保留、大响应句柄直通、异步完成观察及重启恢复。`CPR_PLUGIN_TEST_BINARY` 可指定安装包解出的二进制，复用同一套 RPC 验收

实际安装、页面和协议验证使用官方下载的原版 CPR 3.43.0、独立 PostgreSQL、Redis 与本地模拟上游。临时配置、测试凭据、原始截图和验收脚本放在忽略目录 `.runtime/`，不在正式源码中保存模拟记录或真实凭据。交付截图与验证范围见[验收记录](validation.md)
