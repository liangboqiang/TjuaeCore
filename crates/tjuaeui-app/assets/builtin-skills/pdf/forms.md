> **关键要求：必须按顺序完成以下步骤。在完成检查前，不得直接编写填表代码。**

# PDF 表单填写

首先检查 PDF 是否包含可填写的 AcroForm 字段。在本文件所在目录运行：

```bash
python scripts/check_fillable_fields.py <input.pdf>
```

## 可填写表单

如果 PDF 包含可填写字段，按以下流程操作。

### 1. 提取字段信息

```bash
python scripts/extract_form_field_info.py <input.pdf> <field_info.json>
```

生成的 JSON 会列出字段标识、页码、PDF 坐标和类型。PDF 坐标以页面左下角为
原点，矩形格式为 `[left, bottom, right, top]`。

```jsonc
[
  {
    "field_id": "last_name",
    "page": 1,
    "rect": [100, 500, 280, 520],
    "type": "text"
  },
  {
    "field_id": "adult",
    "page": 1,
    "type": "checkbox",
    "checked_value": "/On",
    "unchecked_value": "/Off"
  },
  {
    "field_id": "contact_method",
    "page": 1,
    "type": "radio_group",
    "radio_options": [
      {
        "value": "email",
        "rect": [100, 450, 115, 465]
      }
    ]
  },
  {
    "field_id": "country",
    "page": 1,
    "type": "choice",
    "choice_options": [
      {
        "value": "CN",
        "text": "中国"
      }
    ]
  }
]
```

### 2. 识别字段用途

将每页转换为 PNG：

```bash
python scripts/convert_pdf_to_images.py <input.pdf> <output_directory>
```

逐页查看图片，并结合 `field_info.json` 判断每个字段的用途。注意把 PDF 坐标
转换为图片坐标。

### 3. 准备字段值

创建 `field_values.json`：

```jsonc
[
  {
    "field_id": "last_name",
    "description": "用户的姓氏",
    "page": 1,
    "value": "张"
  },
  {
    "field_id": "adult",
    "description": "用户年满 18 岁时勾选",
    "page": 1,
    "value": "/On"
  }
]
```

- `field_id` 和 `page` 必须与提取结果完全一致。
- 复选框使用字段提供的 `checked_value` 或 `unchecked_value`。
- 单选组只能使用 `radio_options` 中的 `value`。
- 选择字段只能使用 `choice_options` 中的 `value`。

### 4. 填写并验证

```bash
python scripts/fill_fillable_fields.py \
  <input.pdf> <field_values.json> <output.pdf>
```

脚本会验证字段标识和值。出现错误时，修正对应字段后重新运行。完成后必须渲染
输出 PDF 并目视检查每一页。

## 不可填写表单

如果 PDF 不包含可填写字段，需要以文本标注方式填写。必须依次完成下面四步：

1. 将 PDF 转换为图片并识别字段边界框。
2. 创建 `fields.json` 和边界框验证图。
3. 自动检查并目视确认全部边界框。
4. 使用已验证的边界框填写 PDF。

### 第 1 步：视觉分析

```bash
python scripts/convert_pdf_to_images.py <input.pdf> <output_directory>
```

检查每一页，为字段标签和输入区域分别确定边界框：

- 标签框与输入框不得相交。
- 输入框只能覆盖实际填写区域，并应足够容纳完整文本。
- 标签位于框内时，输入区域从标签右侧延伸到框边缘。
- 标签位于横线上方或下方时，输入区域应覆盖整条横线。
- 对复选框，只框选小方框，不得把“是”“否”等标签文字包含在输入框内。

### 第 2 步：创建字段文件与验证图

创建 `fields.json`。图片坐标以左上角为原点，边界框格式为
`[left, top, right, bottom]`：

```jsonc
{
  "pages": [
    {
      "page_number": 1,
      "image_width": 1240,
      "image_height": 1754
    }
  ],
  "form_fields": [
    {
      "page_number": 1,
      "description": "在此填写用户姓氏",
      "field_label": "姓氏",
      "label_bounding_box": [30, 125, 95, 142],
      "entry_bounding_box": [100, 125, 280, 142],
      "entry_text": {
        "text": "张",
        "font_size": 14,
        "font_color": "000000"
      }
    },
    {
      "page_number": 1,
      "description": "用户年满 18 岁时勾选",
      "field_label": "是",
      "label_bounding_box": [100, 525, 132, 540],
      "entry_bounding_box": [140, 525, 155, 540],
      "entry_text": {
        "text": "X"
      }
    }
  ]
}
```

为每一页生成验证图：

```bash
python scripts/create_validation_image.py \
  <page_number> <fields.json> <input_image> <output_image>
```

验证图用红框标记输入区域，用蓝框标记标签。

### 第 3 步：验证边界框

先执行自动检查：

```bash
python scripts/check_bounding_boxes.py <fields.json>
```

如果存在相交、越界或高度不足等错误，重新分析并修正，直到脚本不再报错。

随后必须目视检查每张验证图：

- 红框只能覆盖输入区域，内部不得包含原有文字。
- 蓝框应完整覆盖对应标签。
- 复选框的红框必须居中覆盖小方框，蓝框覆盖标签文字。

发现任何偏差都要修改 `fields.json`、重新生成验证图并再次检查。

### 第 4 步：添加标注

```bash
python scripts/fill_pdf_form_with_annotations.py \
  <input.pdf> <fields.json> <output.pdf>
```

最后将输出 PDF 渲染为图片并逐页确认：文字未被裁剪、字段位置正确、复选框清晰，
且没有覆盖原表单内容。
