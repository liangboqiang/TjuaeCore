# TjuaeCore

TjuaeCore 是 Tjuae 的本地服务层，负责会话、数据、智能体、MCP、扩展、定时任务、
文件处理和系统能力。它通过 HTTP 与 WebSocket 向 TjuaeUI 提供统一后端接口，
并通过 TjuaeCLI SDK 运行内置智能体。

当前品牌首版为 `0.2.0`。

## 仓库关系

Tjuae 由四个独立仓库组成：

- [TjuaeUI](https://github.com/liangboqiang/TjuaeUI)：桌面、移动和 Web 入口。
- [TjuaeCore](https://github.com/liangboqiang/TjuaeCore)：本仓库，本地服务和系统能力。
- [TjuaeCLI](https://github.com/liangboqiang/TjuaeCLI)：命令行客户端与智能体 SDK。
- [TjuaeHub](https://github.com/liangboqiang/TjuaeHub)：扩展 Schema、目录和构建工具。

依赖方向为 `UI → Core → CLI SDK`。Core 可读取 Hub 提供的扩展 Schema 和目录
数据，不依赖 UI 的具体实现。

## 环境要求

- Rust `1.95.0`，以 `rust-toolchain.toml` 为准。
- [just](https://github.com/casey/just)。
- TjuaeCLI SDK 可由 `Cargo.toml` 指定的 Git 版本提供；联仓开发时也可通过项目
  脚本使用相邻的 `../TjuaeCLI`。

## 本地开发

```bash
just setup
just run --local
```

默认二进制名为 `tjuaecore`。常用参数：

```bash
just run -- --help
just run -- --host 127.0.0.1 --port 56666 --data-dir data
just run -- doctor
```

运行时数据写入 `--data-dir` 指定目录；工作目录可通过 `--work-dir` 或
`TJUAE_WORK_DIR` 设置。

## 常用命令

```bash
just fmt-check
just lint
just test
just migration-check
just check
```

提交前至少执行受影响 crate 的格式、Clippy 和测试。完整规则见
[AGENTS.md](./AGENTS.md)，架构说明见 [ARCHITECTURE.md](./ARCHITECTURE.md)。

数据库 Migration 一旦发布不得修改。品牌首版重建 Migration 基线时，必须显式
使用仓库门禁认可的例外开关，并在提交说明中记录原因。

## 扩展契约

- Manifest：`tjuae-extension.json`
- Engine 字段：`engine.tjuae`
- 扩展搜索路径：`TJUAE_EXTENSIONS_PATH`
- 扩展状态文件：`TJUAE_EXTENSION_STATES_FILE`

本仓库不提供旧名称、旧配置路径、旧协议字段或旧扩展格式的运行时兼容层。

## 发布

Release tag 使用 `v<version>`。`v0.2.0` 对应的产物命名为：

```text
tjuaecore-v0.2.0-<target>.tar.gz
tjuaecore-v0.2.0-<target>.zip
```

发布流程不依赖外部发版服务：主分支通过 `just verify` 和安全审计后，
直接创建并推送与工作区版本一致的标签，GitHub Actions 会构建六个目标并发布
带 SHA-256 校验和的资产。必要时也可在“发布”工作流中手动输入标签重建。
