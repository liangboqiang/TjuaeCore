# PDF 高级处理参考

本文补充主技能中未展开的高级 PDF 处理方案。选择工具时优先考虑任务类型、
性能和运行环境。

## pypdfium2

`pypdfium2` 是 PDFium 的 Python 绑定，适合快速渲染、生成图片，也可在不能使用
PyMuPDF 时作为替代方案。

### 渲染为图片

```python
import pypdfium2 as pdfium

pdf = pdfium.PdfDocument("document.pdf")

# 渲染第一页；scale 越大，输出分辨率越高。
page = pdf[0]
bitmap = page.render(scale=2.0, rotation=0)
bitmap.to_pil().save("page_1.png", "PNG")

# 批量渲染。
for index, page in enumerate(pdf):
    image = page.render(scale=1.5).to_pil()
    image.save(f"page_{index + 1}.jpg", "JPEG", quality=90)
```

### 提取文本

```python
import pypdfium2 as pdfium

pdf = pdfium.PdfDocument("document.pdf")
for index, page in enumerate(pdf):
    text = page.get_textpage().get_text_range()
    print(f"第 {index + 1} 页：{len(text)} 个字符")
```

## JavaScript 库

### pdf-lib

`pdf-lib` 可在 Node.js 和浏览器环境中创建或修改 PDF。

#### 读取并修改已有 PDF

```javascript
import { PDFDocument } from 'pdf-lib';
import fs from 'fs';

const bytes = fs.readFileSync('input.pdf');
const document = await PDFDocument.load(bytes);
const page = document.addPage([600, 400]);

page.drawText('由 pdf-lib 添加', {
  x: 50,
  y: 350,
  size: 16,
});

fs.writeFileSync('modified.pdf', await document.save());
```

#### 从零创建 PDF

```javascript
import { PDFDocument, StandardFonts, rgb } from 'pdf-lib';
import fs from 'fs';

const document = await PDFDocument.create();
const font = await document.embedFont(StandardFonts.Helvetica);
const page = document.addPage([595, 842]);
const { height } = page.getSize();

page.drawText('Invoice #12345', {
  x: 40,
  y: height - 50,
  size: 18,
  font,
  color: rgb(0.2, 0.2, 0.8),
});

page.drawRectangle({
  x: 40,
  y: height - 100,
  width: 515,
  height: 30,
  color: rgb(0.9, 0.9, 0.9),
});

fs.writeFileSync('created.pdf', await document.save());
```

#### 合并与拆分

```javascript
import { PDFDocument } from 'pdf-lib';
import fs from 'fs';

const merged = await PDFDocument.create();
for (const path of ['doc1.pdf', 'doc2.pdf']) {
  const source = await PDFDocument.load(fs.readFileSync(path));
  const pages = await merged.copyPages(source, source.getPageIndices());
  pages.forEach((page) => merged.addPage(page));
}
fs.writeFileSync('merged.pdf', await merged.save());
```

拆分时为每个目标文件创建新的 `PDFDocument`，再用 `copyPages` 复制所需页。

### pdfjs-dist

`pdfjs-dist` 适合在 JavaScript 中渲染页面、提取带坐标文本和读取标注。

```javascript
import * as pdfjsLib from 'pdfjs-dist/legacy/build/pdf.mjs';

const task = pdfjsLib.getDocument('document.pdf');
const document = await task.promise;
const page = await document.getPage(1);
const viewport = page.getViewport({ scale: 2 });
const text = await page.getTextContent();
const annotations = await page.getAnnotations();

console.log({
  pages: document.numPages,
  width: viewport.width,
  height: viewport.height,
  textItems: text.items,
  annotations,
});
```

在 Node.js 渲染位图时还需要提供兼容 Canvas 的实现；只提取文本和标注时通常
不需要 Canvas。

## 高级命令行操作

### poppler-utils

```bash
# 提取带边界框坐标的文本，XML 中包含每个文本元素的精确位置。
pdftotext -bbox-layout input.pdf output.xml

# 按 300 DPI 将全部页面转换为 PNG。
pdftoppm -png -r 300 input.pdf page

# 只转换第 2–5 页。
pdftoppm -f 2 -l 5 -png -r 300 input.pdf page

# 以指定质量生成 JPEG。
pdftoppm -jpeg -jpegopt quality=90 -r 200 input.pdf page

# 提取嵌入图片并保留原格式。
pdfimages -all input.pdf image

# 只列出图片信息。
pdfimages -list input.pdf
```

### qpdf

```bash
# 每十页拆分为一个文件。
qpdf input.pdf --split-pages=10 output-%d.pdf

# 提取不连续页。
qpdf input.pdf --pages . 1-3,7,10-z -- selected.pdf

# 从多个文件选择页面后合并。
qpdf --empty --pages first.pdf 1-5 second.pdf 2,4 -- merged.pdf

# 线性化，便于网络流式读取。
qpdf --linearize input.pdf optimized.pdf

# 压缩对象流并清理未引用对象。
qpdf --object-streams=generate --remove-unreferenced-resources=yes \
  input.pdf compact.pdf

# 尝试重写损坏的结构。
qpdf input.pdf repaired.pdf

# 查看结构信息。
qpdf --check input.pdf

# 设置密码和权限。
qpdf --encrypt USER_PASSWORD OWNER_PASSWORD 256 \
  --print=none --modify=none -- input.pdf encrypted.pdf

# 查看加密状态。
qpdf --show-encryption encrypted.pdf

# 解密。
qpdf --password=USER_PASSWORD --decrypt encrypted.pdf decrypted.pdf
```

## 高级 Python 技巧

### 使用 pdfplumber 获取精确坐标

```python
import pdfplumber

with pdfplumber.open("document.pdf") as pdf:
    for page in pdf.pages:
        for word in page.extract_words():
            print(
                word["text"],
                word["x0"],
                word["top"],
                word["x1"],
                word["bottom"],
            )
```

### 调整表格识别

```python
import pdfplumber

settings = {
    "vertical_strategy": "lines",
    "horizontal_strategy": "lines",
    "snap_tolerance": 3,
    "join_tolerance": 3,
    "intersection_tolerance": 3,
}

with pdfplumber.open("document.pdf") as pdf:
    for page in pdf.pages:
        for table in page.extract_tables(settings):
            print(table)
```

如果表格没有明确边框，可把策略改为 `"text"`，并按文档版式调整最小词数和容差。

### 使用 reportlab 创建专业报表

```python
from reportlab.lib import colors
from reportlab.lib.pagesizes import A4
from reportlab.lib.styles import getSampleStyleSheet
from reportlab.lib.units import mm
from reportlab.platypus import Paragraph, SimpleDocTemplate, Spacer, Table, TableStyle

document = SimpleDocTemplate("report.pdf", pagesize=A4)
styles = getSampleStyleSheet()
data = [
    ["项目", "数量", "价格"],
    ["Widget", "2", "¥100"],
    ["Gadget", "1", "¥75"],
]
table = Table(data, colWidths=[80 * mm, 30 * mm, 30 * mm])
table.setStyle(TableStyle([
    ("BACKGROUND", (0, 0), (-1, 0), colors.HexColor("#E5E7EB")),
    ("GRID", (0, 0), (-1, -1), 0.5, colors.grey),
    ("ALIGN", (1, 1), (-1, -1), "RIGHT"),
]))
document.build([
    Paragraph("报表", styles["Title"]),
    Spacer(1, 8 * mm),
    table,
])
```

中文内容需要注册并使用支持中文的字体，否则可能出现空白或方框。

## 复杂工作流

### 提取图片

优先使用 `pdfimages -all` 原样提取嵌入图片。只有在需要页面合成结果、裁剪或图像
后处理时，才使用 `pypdfium2` 渲染整页后再处理。

### 批量处理与错误隔离

```python
from pathlib import Path
from pypdf import PdfReader, PdfWriter

input_dir = Path("input")
output_dir = Path("output")
output_dir.mkdir(exist_ok=True)

failures = []
for source in input_dir.glob("*.pdf"):
    try:
        reader = PdfReader(source)
        writer = PdfWriter()
        for page in reader.pages:
            writer.add_page(page)
        with (output_dir / source.name).open("wb") as target:
            writer.write(target)
    except Exception as error:
        failures.append((source.name, str(error)))

for name, error in failures:
    print(f"{name}: {error}")
```

批量任务应逐文件捕获异常，并在末尾汇总失败项，不要因为一个损坏文件而丢失其他
文件的结果。

### 裁剪页面

```python
from pypdf import PdfReader, PdfWriter

reader = PdfReader("input.pdf")
page = reader.pages[0]
page.cropbox.lower_left = (50, 50)
page.cropbox.upper_right = (550, 750)

writer = PdfWriter()
writer.add_page(page)
with open("cropped.pdf", "wb") as output:
    writer.write(output)
```

PDF 裁剪坐标以点为单位，原点通常位于左下角。

## 性能建议

1. 大文件按页或按固定页数分块处理，不要一次渲染全部页面。
2. 只提取文本时使用 `pdftotext` 或 `pdfplumber`，避免不必要的位图渲染。
3. 只提取原始图片时使用 `pdfimages`，通常比整页渲染更快且无质量损失。
4. 填写表单前先判断是否存在 AcroForm 字段；可填写字段比文本标注更可靠。
5. 循环处理大量页面时及时释放页面、位图和图片对象。
6. 输出前根据用途选择合理 DPI：屏幕预览通常为 144–200 DPI，印刷可用
   300 DPI。

## 常见问题

### 加密 PDF

```python
from pypdf import PdfReader

reader = PdfReader("encrypted.pdf")
if reader.is_encrypted:
    result = reader.decrypt("password")
    if result == 0:
        raise ValueError("密码错误")
```

### 损坏的 PDF

先运行 `qpdf --check input.pdf` 获取诊断信息，再尝试
`qpdf input.pdf repaired.pdf`。如果重写失败，应保留原文件并向用户报告，不能
静默丢页。

### 无法提取文本

扫描件通常没有文本层。先渲染为图片，再使用 OCR：

```python
import pytesseract
from pdf2image import convert_from_path

for index, image in enumerate(convert_from_path("scanned.pdf", dpi=300)):
    text = pytesseract.image_to_string(image, lang="chi_sim+eng")
    print(f"第 {index + 1} 页\n{text}")
```

### 字体或字符缺失

创建 PDF 时嵌入覆盖目标字符集的字体。合并或修改已有文档时，避免假设所有页面
共享同一字体资源。
