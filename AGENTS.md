# TjuaeCore 智能体协作规范

本文件只记录 AI 智能体和贡献者必须执行的规则。实现原理与设计背景请查阅
[ARCHITECTURE.md](./ARCHITECTURE.md)。

## 最高优先级规则

### 不得猜测 Agent CLI 行为

不得根据 CLI 名称、经验或“看起来应该如此”来推断 `claude`、`codex`、
`gemini`、`opencode`、`hermes`、`tjuae-cli` 等程序的协议、字段语义、时序、
默认值或能力。相关结论必须明确引用以下至少一种一手证据：

1. `~/tjuae/protocols/samples/` 中真实采集的协议流量。
2. 本机 Cargo registry 中 `agent-client-protocol` 与
   `agent-client-protocol-schema` 的源码。
3. 官方适配器或 CLI 自身生成的 Schema，例如
   `samples/codex-cli/<ver>/schema-full/`。
4. CLI 自身的 `--help`、自描述 Schema，或从真实 CLI 录制并通过的集成夹具。

若证据不足，必须写明“尚未验证，需要抓包或读取 Schema”，不得把推测写成
事实。说明 CLI 行为时，应在同一段中标出证据路径。

### 未亲自核验，不得断言

- 子智能体报告只能作为线索；转述关键结论前必须打开其引用的文件和行号核验。
- 声称某项能力“不存在”前，必须先搜索相关符号，再阅读实际入口和处理器。
- 测试输入必须真正覆盖所声称验证的事件和路径。只覆盖
  `start/text/finish` 的简单用例不能证明工具调用、权限、子智能体或模式切换。
- 跨层问题必须沿后端、协议、前端逐层追踪，定位真实分歧点。
- 结论应使用“已验证：<文件:行号>”“尚未检查”或“子智能体报告，未核验”等
  与证据强度一致的表达。

## 日志

修改关键路径或难以观察的流程时，应明确评估是否需要补充日志。简单重构、
纯测试修改和文案修改通常无需新增日志。

- `debug`：高频、开发期细节和状态转移。
- `info`：低频生命周期边界、重要状态变化和非敏感关联信息。
- `warn`：已被安全处理的异常或格式错误数据。
- `error`：契约破坏或操作失败。

生产日志不得包含提示词、工具输入输出、文件内容、命令正文、令牌、密钥或原始
供应商请求/响应。确需本地调试时，必须置于显式开发开关之后且默认关闭。

## 架构

Cargo workspace 分为 Foundation → Capability → Domain → Composition 四层，
依赖只能向下流动。

- 上层可依赖下层。
- 同层模块只能通过 trait 抽象协作。
- 下层不得依赖上层。
- 禁止循环依赖。
- 修改基础层 crate 前必须评估影响范围。

### 领域 crate 结构

- `lib.rs`：只导出模块，不放业务逻辑。
- `routes.rs`：导出 `domain_routes(state) -> Router`，Handler 只做请求与响应转换。
- `service.rs`：业务逻辑唯一归属，不得导入 `axum`。
- `state.rs`：定义 `#[derive(Clone)]` 的 RouterState，并用 `Arc` 持有依赖。

### API 约定

- 路由前缀使用 `/api/`，资源名使用 kebab-case。
- 成功响应使用 `ApiResponse<T>`，失败响应使用 `ErrorResponse`。
- 请求和响应类型统一定义在 `tjuaeui-api-types`。
- `tjuaeui-api-types` 不得依赖 `axum`、`tower` 等 HTTP 框架。
- `tjuaeui_common::ApiError` 只用于路由和中间件等 HTTP 边界；领域服务使用本
  crate 错误，并在路由层映射。

### WebSocket 事件

- 名称格式为 `domain.camelCaseAction`。
- 消息类型使用 `WebSocketMessage<T>`。
- 新事件不得沿用旧的 kebab-case 或三级名称。

### 数据层

- Repository trait 位于 `tjuaeui-db`，以 `I` 开头。
- SQLite 实现以 `Sqlite` 开头。
- Row model 放在 `tjuaeui-db/src/models/`。
- 参数对象与对应 repository 同文件。
- Migration 命名为 `NNN_descriptive_name.sql`，不得手工修改数据库。
- Service 依赖 trait，不直接依赖具体实现。

### 依赖注入

- `AppServices` 是唯一的服务构造中心。
- 领域 crate 只定义 RouterState，不在内部构造依赖。
- 组装统一在 `tjuaeui-app` 的 `build_*_state()` 中完成。

### 安全

- 新接口必须评估是否需要认证中间件。
- 状态修改操作必须受 CSRF 保护。
- 敏感操作应评估限流。
- 错误响应不得泄漏内部细节。
- 禁止硬编码密钥和令牌。

## 代码风格

- 使用 Rust 2024 edition 和 `rust-toolchain.toml` 固定的稳定工具链。
- 代码注释、提交说明和面向维护者的文档使用中文；协议字段、类型名和命令保持原样。
- 每个 `.rs` 文件遵守单一职责。
- 单个 `.rs` 文件建议少于 1000 行；超出时优先拆分模块，测试文件除外。

## 开发流程

### 子进程

新增子进程必须通过 `tjuaeui_runtime::Builder` 启动，禁止直接使用
`tokio::process::Command`。详见
[运行时基础设施](./ARCHITECTURE.md#运行时基础设施)。

### 推送

必须使用 `just push`，不得直接执行 `git push`。该命令会先执行 Migration、
Lint、格式和测试门禁，并透传普通 push 参数。

### 在现有领域新增接口

1. 在 `tjuaeui-api-types/src/{domain}.rs` 定义请求和响应。
2. 在 `crates/tjuaeui-{domain}/src/routes.rs` 添加 Handler。
3. 在 `crates/tjuaeui-{domain}/src/service.rs` 实现业务逻辑。
4. 在 `domain_routes()` 注册路由。
5. 在领域 crate 或 `tjuaeui-app/tests/` 添加测试。

### 新增 Migration

1. 查看 `crates/tjuaeui-db/migrations/` 的最大编号。
2. 创建 `NNN_descriptive_name.sql`。
3. 可重复创建的对象应使用 `IF NOT EXISTS`。

### 新增 WebSocket 事件

1. 在 `tjuaeui-api-types` 定义事件类型。
2. 在 Service 中通过 `event_bus.broadcast()` 发送。
3. 名称遵守 `domain.camelCaseAction`。

## 测试

| 位置 | 用途 |
| --- | --- |
| 源文件内的 `#[cfg(test)]` | 模块内部单元测试 |
| `crates/<crate>/tests/` | 该 crate 的集成或端到端测试 |

- 数据库测试使用 `init_database_memory()`。
- 优先使用真实内存数据库；只有隔离无关依赖时才使用 Mock。
- 新功能必须包含测试。

### 覆盖要求

正常路径必须覆盖被修改功能的完整流程。认证、消息收发、Agent 会话、文件上传
下载和 WebSocket 事件属于必须覆盖的关键路径。

错误路径至少包括：

- 缺失字段、错误类型或超限内容；
- 资源不存在；
- 未认证、越权或跨用户访问；
- 重复创建、状态不允许等业务规则冲突。

错误测试必须断言明确状态码、错误码或消息，不得只断言“失败”。

涉及认证、授权或数据隔离的接口还必须验证：

- 未认证请求被拒绝；
- 用户数据相互隔离；
- 缺失或错误 CSRF 令牌的写操作被拒绝；
- 响应不包含密码、令牌等敏感字段。

新增 WebSocket 事件必须验证触发时机、`WebSocketMessage<T>` 结构和订阅者隔离。

### 测试失败处理

测试失败时先判断断言是否仍代表正确需求：

1. 断言仍正确：修实现，不改测试。
2. 接口确实有意变化：确认变更意图后更新测试，并保留有意义的断言。
3. 无法确认：停止修改，追踪需求和调用链。

禁止删除失败测试来“修复”问题，也禁止把精确断言弱化为模糊的成功判断。

## 验证策略

开发过程中只验证正在修改的 crate：

```bash
cargo test -p tjuaeui-<crate>
cargo clippy -p tjuaeui-<crate> -- -D warnings
```

提交前验证所有受影响 crate：

```bash
cargo fmt --all -- --check
cargo clippy -p tjuaeui-<crate1> -p tjuaeui-<crate2> -- -D warnings
cargo test -p tjuaeui-<crate1> -p tjuaeui-<crate2>
```

实现全部完成后再执行 `cargo test --workspace`。完整 workspace 的 Clippy 和测试
耗时较长，应在后台运行。推送前执行：

```bash
just push
```
