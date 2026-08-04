# TjuaeCore

TjuaeCore 是 Tjuae 的本地服务与资产事实层，负责会话、四类本地资产、运行绑定、Trace、
MCP、定时任务、文件处理和系统能力。它通过 HTTP 与 WebSocket 向 TjuaeUI 提供统一接口，
并通过固定版本的 TjuaeCLI SDK 执行智能体会话。

当前候选版本为 `0.3.0`。

## 仓库关系

Tjuae 由四个独立仓库组成：

- [TjuaeUI](https://github.com/liangboqiang/TjuaeUI)：桌面、移动和 Web 入口。
- [TjuaeCore](https://github.com/liangboqiang/TjuaeCore)：本仓库，本地服务和系统能力。
- [TjuaeCLI](https://github.com/liangboqiang/TjuaeCLI)：命令行客户端与智能体 SDK。
- [TjuaeHub](https://github.com/liangboqiang/TjuaeHub)：四类远程原子资产、Schema、审核和分发。

运行依赖方向为 `UI → Core → CLI SDK`。Hub 是独立远程库；Core 按不可变 `dist` 提交读取并
验证市场索引和包，安装后创建用户本地副本。Hub 资产不会越过 Core 直接参与会话运行。

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

## 资产契约

- 四类原子资产固定为 `assistant`、`engineAdapter`、`skill`、`mcp`。
- Core 本地资产与 Hub 远程资产是两份实体，通过 TrackingLink、BaseSnapshot、提交和摘要关联。
- 公开 Definition、用户 typed Overlay 和加密 Credential 分离；秘密不会写入 Hub、Trace 或日志。
- 启用使用事务投影与 `AssetRuntimeBinding`；引擎和 MCP 必须先完成匹配当前版本的试跑。
- 第三方 CLI 由用户自行安装和登录；Core 只从用户 PATH 或显式 Overlay 路径检测，不下载、
  安装、更新、缓存或分发任何第三方 CLI。
- 应用扩展仍可提供主题、频道、WebUI 和设置页，但不能贡献四类原子资产或执行资产生命周期脚本。

本仓库不提供旧资产清单、旧直接 CRUD、旧产品路径或旧协议字段的运行时兼容层。

## 发布

Release tag 使用 `v<version>`。`v0.3.0` 对应的产物命名为：

```text
tjuaecore-v0.3.0-<target>.tar.gz
tjuaecore-v0.3.0-<target>.zip
```

发布流程不依赖外部发版服务：主分支通过 `just verify` 和安全审计后，
直接创建并推送与工作区版本一致的标签，GitHub Actions 会构建六个目标并发布
带 SHA-256 校验和的资产。必要时也可在“发布”工作流中手动输入标签重建。

## 许可与来源

本项目采用 Apache License 2.0。上游来源与保留声明见
[UPSTREAM.md](./UPSTREAM.md) 和 [LICENSE](./LICENSE)。
