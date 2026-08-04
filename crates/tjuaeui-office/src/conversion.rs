use std::{
    io::{Read, Seek},
    path::Path,
};

use calamine::{Data, DataType, Range, Reader, Sheets, open_workbook_auto};
use serde_json::Value;
use tjuaeui_api_types::{
    CellCoord, CellRange, ConversionResultDto, ConversionTarget, DocumentConversionResponse, ExcelSheetData,
    ExcelWorkbookData,
};
use tjuaeui_runtime::Builder as CmdBuilder;
use tracing::warn;

use crate::error::OfficeError;
pub struct ConversionService;

impl ConversionService {
    pub fn new() -> Self {
        Self
    }

    pub async fn convert(
        &self,
        file_path: &str,
        target: ConversionTarget,
    ) -> Result<DocumentConversionResponse, OfficeError> {
        let to_str = match target {
            ConversionTarget::Markdown => "markdown",
            ConversionTarget::ExcelJson => "excel-json",
        };

        let result = match target {
            ConversionTarget::Markdown => self.word_to_markdown(file_path).await,
            ConversionTarget::ExcelJson => self.excel_to_json(file_path),
        };

        let result_dto = match result {
            Ok(data) => ConversionResultDto {
                success: true,
                data: Some(data),
                error: None,
            },
            Err(e) => ConversionResultDto {
                success: false,
                data: None,
                error: Some(e.to_string()),
            },
        };

        Ok(DocumentConversionResponse {
            to: to_str.to_string(),
            result: result_dto,
        })
    }

    async fn word_to_markdown(&self, file_path: &str) -> Result<Value, OfficeError> {
        validate_file_exists(file_path)?;

        let pandoc = find_executable("pandoc");
        let pandoc_path = pandoc.ok_or_else(|| {
            OfficeError::ToolNotFound(
                "pandoc。请在 macOS 上运行 brew install pandoc，\
                 或在 Linux 上运行 apt-get install pandoc"
                    .into(),
            )
        })?;

        let mut builder = CmdBuilder::clean_cli(&pandoc_path);
        builder.args(["-f", "docx", "-t", "markdown", "--wrap=none", file_path]);
        let output = builder
            .output()
            .await
            .map_err(|e| OfficeError::Conversion(format!("运行 pandoc 失败：{e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(OfficeError::Conversion(format!("pandoc 执行失败：{stderr}")));
        }

        let markdown = String::from_utf8_lossy(&output.stdout).into_owned();
        Ok(Value::String(markdown))
    }

    fn excel_to_json(&self, file_path: &str) -> Result<Value, OfficeError> {
        validate_file_exists(file_path)?;

        let mut workbook: Sheets<_> =
            open_workbook_auto(file_path).map_err(|e| OfficeError::Conversion(format!("打开工作簿失败：{e}")))?;

        let sheet_names = workbook.sheet_names().to_vec();
        let mut sheets = Vec::with_capacity(sheet_names.len());

        for name in &sheet_names {
            let range = workbook
                .worksheet_range(name)
                .map_err(|e| OfficeError::Conversion(format!("读取工作表 '{name}' 失败：{e}")))?;

            let data = convert_range_to_2d_array(&range);
            let merges = extract_merge_regions(&mut workbook, name);

            sheets.push(ExcelSheetData {
                name: name.clone(),
                data,
                merges,
                images: None,
            });
        }

        let workbook_data = ExcelWorkbookData { sheets };
        serde_json::to_value(workbook_data).map_err(OfficeError::Json)
    }
}

fn validate_file_exists(file_path: &str) -> Result<(), OfficeError> {
    let path = Path::new(file_path);
    if !path.exists() {
        return Err(OfficeError::Conversion(format!("找不到文件：{file_path}")));
    }
    if !path.is_file() {
        return Err(OfficeError::Conversion(format!("该路径不是文件：{file_path}")));
    }
    Ok(())
}

fn convert_range_to_2d_array(range: &Range<Data>) -> Vec<Vec<Value>> {
    let (rows, cols) = range.get_size();
    let mut data = Vec::with_capacity(rows);

    for r in 0..rows {
        let mut row = Vec::with_capacity(cols);
        for c in 0..cols {
            let cell = &range[(r, c)];
            let value = cell_to_json_value(cell);
            row.push(value);
        }
        data.push(row);
    }

    data
}

fn cell_to_json_value(cell: &Data) -> Value {
    if cell.is_empty() {
        return Value::Null;
    }
    if let Some(b) = cell.get_bool() {
        return Value::Bool(b);
    }
    if let Some(i) = cell.get_int() {
        return Value::Number(i.into());
    }
    if let Some(f) = cell.get_float() {
        return serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(Value::Null);
    }
    if let Some(s) = cell.as_string() {
        return Value::String(s);
    }
    Value::Null
}

fn extract_merge_regions<RS: Read + Seek>(workbook: &mut Sheets<RS>, sheet_name: &str) -> Option<Vec<CellRange>> {
    let xlsx = match workbook {
        Sheets::Xlsx(wb) => wb,
        _ => return None,
    };

    let regions = match xlsx.merge_cells_by_sheet_name(sheet_name) {
        Ok(regions) => regions,
        Err(_) => {
            warn!("failed to load merged regions");
            return None;
        }
    };
    if regions.is_empty() {
        return None;
    }

    let ranges: Vec<CellRange> = regions
        .into_iter()
        .map(|dim| CellRange {
            s: CellCoord {
                r: dim.start.0 as usize,
                c: dim.start.1 as usize,
            },
            e: CellCoord {
                r: dim.end.0 as usize,
                c: dim.end.1 as usize,
            },
        })
        .collect();

    Some(ranges)
}

fn find_executable(name: &str) -> Option<String> {
    which::which(name).ok().map(|p| p.to_string_lossy().into_owned())
}

impl Default for ConversionService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_file_exists_nonexistent() {
        let result = validate_file_exists("/nonexistent/file.xlsx");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("找不到文件"));
    }

    #[test]
    fn validate_file_exists_is_directory() {
        let result = validate_file_exists("/tmp");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("不是文件"));
    }

    #[test]
    fn cell_to_json_value_empty() {
        let cell = calamine::Data::Empty;
        assert_eq!(cell_to_json_value(&cell), Value::Null);
    }

    #[test]
    fn cell_to_json_value_bool() {
        let cell = calamine::Data::Bool(true);
        assert_eq!(cell_to_json_value(&cell), Value::Bool(true));
    }

    #[test]
    fn cell_to_json_value_int() {
        let cell = calamine::Data::Int(42);
        assert_eq!(cell_to_json_value(&cell), serde_json::json!(42));
    }

    #[test]
    #[allow(clippy::approx_constant)] // 3.14 is test data, not an approximation of PI
    fn cell_to_json_value_float() {
        let cell = calamine::Data::Float(3.14);
        let val = cell_to_json_value(&cell);
        assert!(val.is_number());
        let n = val.as_f64().unwrap();
        assert!((n - 3.14).abs() < f64::EPSILON);
    }

    #[test]
    fn cell_to_json_value_string() {
        let cell = calamine::Data::String("hello".to_string());
        assert_eq!(cell_to_json_value(&cell), Value::String("hello".into()));
    }

    #[test]
    fn convert_range_empty() {
        let range = calamine::Range::<calamine::Data>::new((0, 0), (0, 0));
        let data = convert_range_to_2d_array(&range);
        assert_eq!(data.len(), 1);
        assert_eq!(data[0].len(), 1);
    }

    #[test]
    fn conversion_service_new() {
        let _service = ConversionService::new();
    }

    #[tokio::test]
    async fn convert_excel_file_not_found() {
        let svc = ConversionService::new();
        let resp = svc
            .convert("/nonexistent/file.xlsx", ConversionTarget::ExcelJson)
            .await
            .unwrap();
        assert!(!resp.result.success);
        assert!(resp.result.error.as_ref().unwrap().contains("找不到文件"));
        assert_eq!(resp.to, "excel-json");
    }

    #[tokio::test]
    async fn convert_word_file_not_found() {
        let svc = ConversionService::new();
        let resp = svc
            .convert("/nonexistent/file.docx", ConversionTarget::Markdown)
            .await
            .unwrap();
        assert!(!resp.result.success);
        assert!(resp.result.error.as_ref().unwrap().contains("找不到文件"));
        assert_eq!(resp.to, "markdown");
    }
}
