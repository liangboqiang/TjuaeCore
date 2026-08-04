---
name: mermaid
description: 使用 beautiful-mermaid 将 Mermaid 图渲染为 SVG 或 ASCII/Unicode 文本。适用于流程图、时序图、状态图、类图和 ER 图。
---

> **平台提示：**以下命令按 macOS / Linux 的 bash/zsh 编写。执行前先确认
> 操作系统；Windows 上不得原样运行，应转换为 PowerShell：
>
> | bash（macOS/Linux） | PowerShell（Windows） |
> | --- | --- |
> | `a && b` | 分两步执行，或使用 `a; if ($?) { b }` |
> | `cat <<'EOF' \| tool …`（heredoc） | 先写入临时文件，再把文件传给工具 |
> | `VAR=$(cmd)` … `$VAR` | `$VAR = cmd` … `$VAR` |
> | `cmd > /dev/null` | `cmd > $null` |
> | `… \| grep PAT` | `… \| Select-String PAT` |
> | `… \| jq …` | `… \| ConvertFrom-Json`，再读取字段 |
> | `python3 x.py` | `python x.py` 或 `py x.py` |
> | `~/dir`, `/tmp` | `$env:USERPROFILE\dir`, `$env:TEMP` |
> | `cp`、`mkdir -p`、`rm -rf` | `Copy-Item`、`New-Item -ItemType Directory -Force`、`Remove-Item -Recurse -Force` |
>
> 若命令没有直接的 Windows 等价写法，优先使用内置文件或 HTTP 工具。

# Mermaid 图表渲染器

使用 `beautiful-mermaid` 渲染 Mermaid 图，支持五类图表和两种输出模式。

## 快速开始

> 首次运行会自动安装 `beautiful-mermaid`。

### SVG 输出（默认）

```bash
# 从文件读取
npx tsx scripts/render.ts diagram.mmd --output diagram.svg

# 从标准输入读取
echo "graph LR; A-->B-->C" | npx tsx scripts/render.ts --stdin --output flow.svg
```

### ASCII 输出（终端）

```bash
# 输出适合终端显示的文本图
npx tsx scripts/render.ts diagram.mmd --ascii

# 直接通过管道传入
echo "graph TD; Start-->End" | npx tsx scripts/render.ts --stdin --ascii
```

输出示例：

```
┌───────┐     ┌─────┐
│ Start │────▶│ End │
└───────┘     └─────┘
```

## 支持的图表

| 类型 | 语法 | 适用场景 |
| --------- | ----------------- | ----------------------- |
| 流程图 | `graph TD/LR` | 流程与决策 |
| 时序图 | `sequenceDiagram` | API 调用与交互 |
| 状态图 | `stateDiagram-v2` | 状态机 |
| 类图 | `classDiagram` | 面向对象设计 |
| ER 图 | `erDiagram` | 数据库 Schema |

## 主题（仅 SVG）

```bash
npx tsx scripts/render.ts diagram.mmd --theme github-dark --output out.svg
```

传入无效主题名可查看可用主题列表，例如 `--theme ?`。

## 资源

- `scripts/render.ts`：主渲染脚本。
- `references/syntax.md`：Mermaid 语法速查。
