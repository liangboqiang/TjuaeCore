---
name: weixin-file-send
description: |
  当用户要求把本地文件或图片发回当前聊天时使用，例如“把文件发给我”。
---

# 微信文件发送

在以下情况使用：

- 用户要求发回文件或图片，例如：
  - “把文件发给我”
  - “图片发过来”
  - “把结果传给我”
  - "发给我"
  - "图片发过来"
  - "把文件传给我"
- 本地文件已存在，需要发送到当前聊天。

只有输出完整协议块后才能声称文件已发送；缺少协议块时，应用不会实际发送。

## 协议

在最终回复末尾追加一个或多个协议块：

```text
[TJUAE_CHANNEL_SEND]
{"type":"image","path":"./output/chart.png","caption":"Chart ready"}
[/TJUAE_CHANNEL_SEND]
```

```text
[TJUAE_CHANNEL_SEND]
{"type":"file","path":"./output/report.pdf","fileName":"report.pdf","caption":"Report ready"}
[/TJUAE_CHANNEL_SEND]
```

## 规则

- `type` 只能是 `image` 或 `file`。
- `path` 必须指向已存在的真实本地文件。
- 文件位于 workspace 内时使用相对路径。
- `fileName` 仅用于 `file`，可省略；`caption` 也可省略。
- 用户明确要求发送时，应输出协议块，不能只在文字中描述文件。
- 协议块放在用户可见回答之后。
- JSON 不得包在 Markdown 代码围栏中。
- 文件不存在时不得输出协议块。
- 未输出协议块时不得声称文件已发送。

## 示例

带用户提示的图片：

```text
图表已经生成并发送如下。

[TJUAE_CHANNEL_SEND]
{"type":"image","path":"./output/chart.png","caption":"销售图表"}
[/TJUAE_CHANNEL_SEND]
```

只发送文件：

```text
[TJUAE_CHANNEL_SEND]
{"type":"file","path":"./output/report.pdf","fileName":"report.pdf","caption":"周报"}
[/TJUAE_CHANNEL_SEND]
```
