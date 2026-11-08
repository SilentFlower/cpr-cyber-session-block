# 0.1.2 验收记录

验证日期：2026-11-08，平台：Linux x86_64

宿主为官方下载的原版 CPR 3.43.0，提交 `402bc37fbb5c83378cc83cec89e9fa251e4a99c1`，使用独立 PostgreSQL、Redis 与本地 HTTP/SSE、WebSocket 模拟上游。宿主源码和二进制保持原样，没有调用真实 OpenAI

## 构建与测试

| 检查 | 结果 |
| --- | --- |
| `cargo fmt -- --check`、全部 target / feature 严格 Clippy | 通过 |
| `cargo test --locked` | 38 项通过，含 22 项真实插件子进程 RPC 验收 |
| 安装包解出的二进制执行 `tests/protocol.rs` | 22 项通过 |
| 前端冻结锁文件安装、ESLint、Vue 类型检查与生产构建 | 通过 |
| 官方 CLI 打包、SHA-256 与包内 7 项文件摘要 | 通过 |

安装包：`dist/silentflower.cyber-session-block-0.1.2-x86_64-unknown-linux-gnu.tar.gz`

SHA-256：`f4a0c4353b7efe0a629cc30ca3720743746a4fac218aa418e2f9c89d627f8ac0`

## 大响应与终态

原版宿主中的对照结果：

| 插件 | 完成帧字节数 | 客户端最终事件 |
| --- | ---: | --- |
| 0.1.1 | 250000 | `response.completed` |
| 0.1.1 | 274599 | `response.failed` |
| 0.1.2 | 262143、262144、262145、265572、274599、2097152 | 均为 `response.completed` |

HTTP/SSE 与原生 WebSocket 均验证正常结束；WebSocket 同一连接可在大完成帧后继续携带 `previous_response_id` 续接。2 MiB 上游完成帧也通过 HTTP JSON 验收

子进程测试使用 256 KiB RPC 信用窗口，验证 request 与 attempt 返回原始正文句柄，不读取正文、不输出转发流。正常完成不查询错误记录、不写封禁状态

## 封禁与生命周期

- 同步真实策略错误保留原始错误，并停止已经确认命中的后续换号重试
- 流式策略失败由 `request_completed` 观察触发原生记录核实，HTTP/SSE 与 WebSocket 后续请求被拦截，上游只收到首次调用
- 普通 unavailable 不建立封禁，同会话后续请求继续到达模拟上游
- 原生记录尚未落库时保留待核实归属，下一轮准入与维护继续核实
- 异步观察以原始完成时间计算封禁，不因查询延迟延长期限
- 重复完成通知不重复登记；正常完成、取消与初始准备取消回收请求关联
- 手动解除消费已经发生的待核实失败，迟到完成通知不会重新封禁解除前的旧失败；新的真实命中仍可建立记录
- 从 0.1.1 更新到最终安装包后，设置及已保存封禁历史保留

观察只绑定 `request_completed`，没有绑定 `websocket_response`。异步投递和关联缓存的边界见 [开发说明](development.md)

## 页面与交付范围

本次没有页面源码变更，安装包的三项前端资源与 0.1.1 逐字节相同。现有[浅色](screenshots/light.png)、[深色](screenshots/dark.png)、[窄屏](screenshots/mobile.png)与[设置弹窗](screenshots/settings.png)截图来自页面原有验收，本次未重新执行浏览器交互验收

0.1.2 已在隔离原版宿主安装验收。没有使用真实 Codex 客户端或真实模型上游执行端到端请求
