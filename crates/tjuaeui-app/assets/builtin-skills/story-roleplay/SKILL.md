---
name: story-roleplay
description: 解析并应用 PNG、WebP、JSON 格式的角色卡和世界信息，兼容 SillyTavern；支持关键词触发、Character Book 和动态上下文注入。
---

> **平台提示：**以下命令按 macOS / Linux 的 bash/zsh 编写。Windows 上应转换
> 为 PowerShell；`python3` 改为 `python` 或 `py`，`~/` 改为
> `$env:USERPROFILE\`。若没有直接等价语法，优先使用内置文件工具。

# 故事角色扮演

解析角色卡和世界信息，并将其自然应用到故事对话中。

## 强制约束

1. 禁止凭图片外观猜测 PNG/WebP 中的数据，必须用解析器提取元数据。
2. 优先复制 `skills/story-roleplay/scripts/` 中的预置工具；找不到时才创建。
3. 自行创建 `package.json` 时，依赖版本固定为 `1.0.0`，不得使用 `^`。
4. 解析失败时如实报告错误，禁止编造角色或世界设定。

## 解析器工作流

### 1. 复制预置工具

按顺序尝试，成功后停止：

```bash
cp skills/story-roleplay/scripts/parse-character-card.js .
cp skills/story-roleplay/scripts/package.json .
```

若 workspace 不在项目根目录，从上级目录查找：

```bash
for dir in . .. ../.. ../../.. ../../../.. ../../../../..; do
  if [ -f "$dir/skills/story-roleplay/scripts/parse-character-card.js" ]; then
    cp "$dir/skills/story-roleplay/scripts/parse-character-card.js" .
    cp "$dir/skills/story-roleplay/scripts/package.json" .
    break
  fi
done
```

仍找不到时，再在用户目录中搜索，并排除临时目录：

```bash
SCRIPT_PATH=$(find ~ -name "parse-character-card.js" \
  -path "*/skills/story-roleplay/scripts/*" \
  ! -path "*/temp*" ! -path "*/*-temp*" ! -path "*/.webpack/*" \
  2>/dev/null | head -1)
```

确认 `parse-character-card.js` 和 `package.json` 已存在。

### 2. 安装依赖

```bash
npm install
```

确认生成 `node_modules`。失败时检查网络、权限和 Node.js 环境。

### 3. 执行

```bash
# 角色卡
node parse-character-card.js <image-path> <output-json-path>

# 世界信息
node parse-character-card.js <image-path> <output-json-path> --world-info
```

第二个参数必须是输出路径，不得使用 `>` 重定向。文件名含空格或中文时加引号：

```bash
node parse-character-card.js "薇娜丽丝.png" character.json
node parse-character-card.js world-info.png world-info.json --world-info
```

### 4. 验证

- 输出应包含 `Successfully extracted data to: <path>`。
- 检查目标 JSON 存在且可解析。
- `PNG metadata does not contain any text chunks` 表示图片可能不是有效角色卡。
- `Required dependencies not found` 表示应先执行 `npm install`。
- `Image file not found` 表示路径错误。

## 角色卡

触发词包括“角色卡”“加载角色”“解析角色卡”等。

### 支持格式

- PNG：从 tEXt chunk 读取 `chara`（V2）或 `ccv3`（V3），内容为 Base64 JSON。
- WebP：可能在 EXIF/XMP 或文本元数据中。优先转为 PNG 后解析；失败时要求用户
  提供 JSON 或从 SillyTavern 重新导出。
- JSON：直接读取 Tavern Card V2/V3，优先使用。

主要字段：

- `name`：角色名；
- `description`：角色描述；
- `personality`：性格；
- `scenario`：场景；
- `first_mes`：开场白；
- `system_prompt`：行为规则；
- `character_book`：角色专属知识库。

应用时将 `system_prompt` 作为角色规则，以 `first_mes` 开场，并让
`character_book` 条目参与关键词触发。图片解析成功后保存为 `character.json`，
完整保留原始数据。

## 世界信息

触发词包括“世界信息”“世界树”“加载世界设定”等。

JSON 结构示例：

```json
{
  "name": "世界名称",
  "entries": [
    {
      "keys": ["关键词一", "关键词二"],
      "content": "触发后注入的内容",
      "priority": 100,
      "enabled": true
    }
  ]
}
```

PNG 使用 tEXt chunk 的 `naidata`，内容为 Base64 JSON；执行解析器时必须带
`--world-info`。WebP 优先转换为 PNG。

触发逻辑：

1. 检查对话是否包含启用条目的 `keys`。
2. 按 `priority` 从高到低选择内容。
3. 将内容自然融入叙事，不生硬粘贴。

图片解析成功后保存为 `world-info.json`。关键词应具体，避免过宽而频繁误触发。

## 角色资料库（Character Book）

`character_book` 与世界信息格式相似，但只服务于当前角色。加载角色卡时提取其
`entries`；对话中按关键词触发。角色专属条目通常比通用世界信息优先级更高，
两者可以同时使用。

## 文件发现与创建

自动查找：

- 角色卡：`character.png`、`character.webp`、`character.json`、
  `*.character.json`；
- 世界信息：`world-info.png`、`world-info.webp`、`world-info.json`、
  `world.json`。

没有文件时，应主动询问故事类型、背景、角色性格与经历、说话风格、世界规则和
地点。先总结并让用户确认，再创建：

- `character.json`：采用 Tavern Card V2/V3；
- `world-info.json`：为关键概念、地点和规则创建有意义的条目。

## 兼容范围

- SillyTavern PNG 内嵌 JSON；
- 可解析或可转换的 WebP；
- Tavern Card V2/V3 JSON；
- `naidata` 世界信息；
- Character Book。

## 最后备用方案

只有所有预置工具查找方式都失败时，才允许创建文件：

```json
{
  "name": "story-roleplay-parser",
  "version": "1.0.0",
  "description": "角色卡与世界信息解析工具",
  "main": "parse-character-card.js",
  "scripts": {
    "parse": "node parse-character-card.js"
  },
  "dependencies": {
    "png-chunks-extract": "1.0.0",
    "png-chunk-text": "1.0.0"
  }
}
```

完整脚本以 `skills/story-roleplay/scripts/parse-character-card.js` 为准。其核心逻辑
必须提取 PNG chunk、解码 tEXt、查找 `chara`/`ccv3`/`naidata`、Base64 解码并
验证 JSON，同时提供明确错误信息。
