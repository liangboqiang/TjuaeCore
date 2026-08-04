---
name: skill-creator
description: 创建或更新高质量技能的指南，用专门知识、可复用流程、脚本、参考资料和素材扩展智能体能力。用户要求新建、设计、重构或打包技能时使用。
---

> **平台提示：**以下命令按 macOS / Linux 编写。Windows 上使用 `python`
> （或 `py`）代替 `python3`，将 `~/…` 改为 `$env:USERPROFILE\…`，并把管道、
> 重定向和 `&&` 转换为 PowerShell 语法。

# 技能创建指南

技能是自包含的模块化目录，通过领域知识、操作流程和工具让通用智能体可靠完成
特定任务。

## 技能能提供什么

1. 多步骤领域流程；
2. 文件格式、API 或外部工具的集成说明；
3. 企业 Schema、业务规则和专门知识；
4. 可复用的脚本、参考资料和输出素材。

## 设计原则

### 保持精炼

技能与系统提示、对话历史、其他技能和用户请求共享上下文。默认假设智能体已具备
通用知识，只写任务所必需且不明显的内容。每段都应能回答：

- 这段信息是否真的影响执行？
- 它的 Token 成本是否值得？

优先给短而准确的示例，不写长篇背景介绍。

### 选择合适的自由度

- 高自由度：多种方法都有效，判断依赖上下文时，用文字原则。
- 中自由度：存在推荐模式但允许变化时，用伪代码或参数化脚本。
- 低自由度：操作脆弱、易错且顺序关键时，提供固定脚本和少量参数。

## 目录结构

```text
skill-name/
├── SKILL.md
│   ├── YAML frontmatter
│   │   ├── name
│   │   └── description
│   └── Markdown 指令
├── scripts/       # 可选：可执行脚本
├── references/    # 可选：按需读取的资料
└── assets/        # 可选：模板、图标、字体等输出素材
```

### `SKILL.md`

- `name`：技能名。
- `description`：主要触发入口，必须同时说明“做什么”和“何时使用”。
- 正文：技能触发后才加载，只放执行流程和资源导航。

### `scripts/`

重复编写相同代码或需要确定性时加入脚本，例如 PDF 旋转。脚本应可直接执行，
并用真实输入测试。不要为了展示结构保留无用示例。

### `references/`

存放 Schema、API 文档、业务政策和详细工作流。资料只在需要时加载，可降低
`SKILL.md` 体积。大于约一万词的文件，应在主技能中给出搜索关键字。

同一信息只保留一份：核心步骤放 `SKILL.md`，细节放 Reference。

### `assets/`

存放最终输出会复用但不需要读入上下文的模板、图片、字体、样板项目等。

### 不应包含的文件

技能目录只保留执行所需内容，不创建额外的 `README.md`、
`INSTALLATION_GUIDE.md`、`QUICK_REFERENCE.md` 或 `CHANGELOG.md`。这些文件会
增加噪声并造成规则分叉。

## 渐进式披露

技能采用三级加载：

1. 始终可见的 `name` 和 `description`；
2. 触发后加载的 `SKILL.md`；
3. 按需读取或直接执行的 Bundled Resource。

`SKILL.md` 建议少于 500 行。接近上限时将变体、详细示例和大段资料拆到
Reference，并在主文件中说明何时读取。

多领域技能可按领域拆分：

```text
bigquery-skill/
├── SKILL.md
└── references/
    ├── finance.md
    ├── sales.md
    ├── product.md
    └── marketing.md
```

多 Provider 技能可按实现拆分：

```text
cloud-deploy/
├── SKILL.md
└── references/
    ├── aws.md
    ├── gcp.md
    └── azure.md
```

Reference 只保持一层，不要形成深层引用链。超过 100 行的 Reference 应提供目录。

## 创建流程

### 1. 用具体示例明确需求

先确认用户会怎样触发技能、期望哪些输入输出、有哪些失败场景。问题不要一次过多，
先问最影响设计的内容。已有技能也应通过真实用例重新核对边界。

完成标志：已经能清楚描述技能要支持和不支持的场景。

### 2. 规划可复用资源

对每个示例回答：

1. 从零执行需要哪些步骤？
2. 哪些代码会重复，适合放进 `scripts/`？
3. 哪些 Schema 或知识应放进 `references/`？
4. 哪些模板或素材应放进 `assets/`？

例如：

- PDF 旋转经常重复，可提供 `scripts/rotate_pdf.py`。
- Web 应用反复需要脚手架，可提供 `assets/frontend-template/`。
- BigQuery 每次都要发现表关系，可提供 `references/schema.md`。

### 3. 初始化

新技能必须先运行：

```bash
scripts/init_skill.py <skill-name> --path <output-directory>
```

已有技能可跳过。脚本会生成目录、Frontmatter 模板和示例资源目录。随后删除不需要
的示例，只保留真实资源。

### 4. 实现与编辑

先实现规划好的脚本、Reference 和 Asset，再编写 `SKILL.md`。新增脚本必须实际
运行验证；大量相似脚本至少验证有代表性的样本。

设计模式参考：

- 多步骤和条件流程：`references/workflows.md`；
- 固定输出格式和质量门槛：`references/output-patterns.md`。

正文使用祈使句或不定式，直接告诉后续智能体做什么。

Frontmatter 的 `description` 必须包含完整触发场景，因为正文只在触发后加载。
不要在正文另写一个无法参与触发的“何时使用”章节。

### 5. 打包

```bash
scripts/package_skill.py <path/to/skill-folder>
```

指定输出目录：

```bash
scripts/package_skill.py <path/to/skill-folder> ./dist
```

打包脚本会先验证 YAML、命名、目录结构、描述质量和资源引用。验证通过后生成
`<skill-name>.skill`；该文件本质为保持目录结构的 Zip。验证失败时先修复错误，
不得绕过。

### 6. 迭代

1. 用技能完成真实任务；
2. 记录卡点、歧义和低效步骤；
3. 判断应修改 `SKILL.md` 还是 Bundled Resource；
4. 实现修改并再次测试。

迭代依据应来自真实使用证据，不为假设场景无限增加规则。
