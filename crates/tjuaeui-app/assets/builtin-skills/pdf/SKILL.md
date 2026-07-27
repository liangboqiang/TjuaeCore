---
name: pdf
description: PDF 综合处理工具：提取文本与表格、创建、合并、拆分、OCR、加水印、加密和填写表单。需要批量处理、生成或分析 PDF 时使用。
license: 专有许可，完整条款见 LICENSE.txt
---

> **平台提示：**以下命令按 macOS / Linux 编写。Windows 上使用 `python`
> （或 `py`）代替 `python3`，将 `~/…` 改为 `$env:USERPROFILE\…`，并把管道、
> 重定向和 `&&` 转换为 PowerShell 语法。脚本本身跨平台。

# PDF 处理指南

## 概览

本指南介绍使用 Python 库和命令行工具完成常见 PDF 操作。高级功能、
JavaScript 库和完整示例见 `reference.md`；填写 PDF 表单前必须阅读
`forms.md`。

## 快速开始

```python
from pypdf import PdfReader, PdfWriter

# 读取 PDF
reader = PdfReader("document.pdf")
print(f"Pages: {len(reader.pages)}")

# 提取文本
text = ""
for page in reader.pages:
    text += page.extract_text()
```

## Python 库

### pypdf：基础操作

#### 合并 PDF

```python
from pypdf import PdfWriter, PdfReader

writer = PdfWriter()
for pdf_file in ["doc1.pdf", "doc2.pdf", "doc3.pdf"]:
    reader = PdfReader(pdf_file)
    for page in reader.pages:
        writer.add_page(page)

with open("merged.pdf", "wb") as output:
    writer.write(output)
```

#### 拆分 PDF

```python
reader = PdfReader("input.pdf")
for i, page in enumerate(reader.pages):
    writer = PdfWriter()
    writer.add_page(page)
    with open(f"page_{i+1}.pdf", "wb") as output:
        writer.write(output)
```

#### 提取元数据

```python
reader = PdfReader("document.pdf")
meta = reader.metadata
print(f"Title: {meta.title}")
print(f"Author: {meta.author}")
print(f"Subject: {meta.subject}")
print(f"Creator: {meta.creator}")
```

#### 旋转页面

```python
reader = PdfReader("input.pdf")
writer = PdfWriter()

page = reader.pages[0]
page.rotate(90)  # Rotate 90 degrees clockwise
writer.add_page(page)

with open("rotated.pdf", "wb") as output:
    writer.write(output)
```

### pdfplumber：提取文本和表格

#### 按版式提取文本

```python
import pdfplumber

with pdfplumber.open("document.pdf") as pdf:
    for page in pdf.pages:
        text = page.extract_text()
        print(text)
```

#### 提取表格

```python
with pdfplumber.open("document.pdf") as pdf:
    for i, page in enumerate(pdf.pages):
        tables = page.extract_tables()
        for j, table in enumerate(tables):
            print(f"Table {j+1} on page {i+1}:")
            for row in table:
                print(row)
```

#### 高级表格提取

```python
import pandas as pd

with pdfplumber.open("document.pdf") as pdf:
    all_tables = []
    for page in pdf.pages:
        tables = page.extract_tables()
        for table in tables:
            if table:  # Check if table is not empty
                df = pd.DataFrame(table[1:], columns=table[0])
                all_tables.append(df)

# 合并全部表格
if all_tables:
    combined_df = pd.concat(all_tables, ignore_index=True)
    combined_df.to_excel("extracted_tables.xlsx", index=False)
```

### reportlab：创建 PDF

#### 创建基础 PDF

```python
from reportlab.lib.pagesizes import letter
from reportlab.pdfgen import canvas

c = canvas.Canvas("hello.pdf", pagesize=letter)
width, height = letter

# 添加文本
c.drawString(100, height - 100, "Hello World!")
c.drawString(100, height - 120, "这是使用 reportlab 创建的 PDF")

# 添加直线
c.line(100, height - 140, 400, height - 140)

# 保存
c.save()
```

#### 创建多页 PDF

```python
from reportlab.lib.pagesizes import letter
from reportlab.platypus import SimpleDocTemplate, Paragraph, Spacer, PageBreak
from reportlab.lib.styles import getSampleStyleSheet

doc = SimpleDocTemplate("report.pdf", pagesize=letter)
styles = getSampleStyleSheet()
story = []

# 添加内容
title = Paragraph("Report Title", styles['Title'])
story.append(title)
story.append(Spacer(1, 12))

body = Paragraph("这是报告正文。" * 20, styles['Normal'])
story.append(body)
story.append(PageBreak())

# 第 2 页
story.append(Paragraph("Page 2", styles['Heading1']))
story.append(Paragraph("Content for page 2", styles['Normal']))

# 构建 PDF
doc.build(story)
```

## 命令行工具

### pdftotext (poppler-utils)

```bash
# 提取文本
pdftotext input.pdf output.txt

# 提取文本并保留布局
pdftotext -layout input.pdf output.txt

# 提取指定页面
pdftotext -f 1 -l 5 input.pdf output.txt  # Pages 1-5
```

### qpdf

```bash
# 合并 PDF
qpdf --empty --pages file1.pdf file2.pdf -- merged.pdf

# 拆分页面
qpdf input.pdf --pages . 1-5 -- pages1-5.pdf
qpdf input.pdf --pages . 6-10 -- pages6-10.pdf

# 旋转页面
qpdf input.pdf output.pdf --rotate=+90:1  # Rotate page 1 by 90 degrees

# 移除密码
qpdf --password=mypassword --decrypt encrypted.pdf decrypted.pdf
```

### pdftk（若可用）

```bash
# 合并
pdftk file1.pdf file2.pdf cat output merged.pdf

# 拆分
pdftk input.pdf burst

# 旋转
pdftk input.pdf rotate 1east output rotated.pdf
```

## 常见任务

### 从扫描件提取文本

```python
# 依赖：pip install pytesseract pdf2image
import pytesseract
from pdf2image import convert_from_path

# 将 PDF 转换为图片
images = convert_from_path('scanned.pdf')

# 对每一页执行 OCR
text = ""
for i, image in enumerate(images):
    text += f"Page {i+1}:\n"
    text += pytesseract.image_to_string(image)
    text += "\n\n"

print(text)
```

### 添加水印

```python
from pypdf import PdfReader, PdfWriter

# 创建水印（或加载已有水印）
watermark = PdfReader("watermark.pdf").pages[0]

# 应用到所有页面
reader = PdfReader("document.pdf")
writer = PdfWriter()

for page in reader.pages:
    page.merge_page(watermark)
    writer.add_page(page)

with open("watermarked.pdf", "wb") as output:
    writer.write(output)
```

### 提取图片

```bash
# 使用 pdfimages（poppler-utils）
pdfimages -j input.pdf output_prefix

# 图片将导出为 output_prefix-000.jpg、output_prefix-001.jpg 等
```

### 密码保护

```python
from pypdf import PdfReader, PdfWriter

reader = PdfReader("input.pdf")
writer = PdfWriter()

for page in reader.pages:
    writer.add_page(page)

# 添加密码
writer.encrypt("userpassword", "ownerpassword")

with open("encrypted.pdf", "wb") as output:
    writer.write(output)
```

## 速查表

| 任务 | 推荐工具 | 命令或代码 |
| ------------------ | ------------------------------- | -------------------------- |
| 合并 PDF | pypdf | `writer.add_page(page)` |
| 拆分 PDF | pypdf | 每页写入一个文件 |
| 提取文本 | pdfplumber | `page.extract_text()` |
| 提取表格 | pdfplumber | `page.extract_tables()` |
| 创建 PDF | reportlab | Canvas 或 Platypus |
| 命令行合并 | qpdf | `qpdf --empty --pages ...` |
| 扫描件 OCR | pytesseract | 先转换为图片 |
| 填写表单 | pdf-lib 或 pypdf | 见 `forms.md` |

## 后续资料

- pypdfium2 高级用法见 `reference.md`。
- JavaScript 的 pdf-lib 用法见 `reference.md`。
- PDF 表单填写遵循 `forms.md`。
- 故障排查见 `reference.md`。
