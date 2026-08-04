use serde::{Deserialize, Serialize};
use tjuaeui_common::PreviewContentType;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PreviewHistoryTargetDto {
    pub content_type: PreviewContentType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PreviewSnapshotInfoDto {
    pub id: String,
    pub label: String,
    pub created_at: i64,
    pub size: u64,
    pub content_type: PreviewContentType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SaveSnapshotRequest {
    pub target: PreviewHistoryTargetDto,
    pub content: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListSnapshotsRequest {
    pub target: PreviewHistoryTargetDto,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GetSnapshotContentRequest {
    pub target: PreviewHistoryTargetDto,
    pub snapshot_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SnapshotContentResponse {
    pub snapshot: PreviewSnapshotInfoDto,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConversionTarget {
    #[serde(rename = "markdown")]
    Markdown,
    #[serde(rename = "excel-json")]
    ExcelJson,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DocumentConversionRequest {
    pub file_path: String,
    pub to: ConversionTarget,
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DocumentConversionResponse {
    pub to: String,
    pub result: ConversionResultDto,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConversionResultDto {
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExcelWorkbookData {
    pub sheets: Vec<ExcelSheetData>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExcelSheetData {
    pub name: String,
    pub data: Vec<Vec<serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merges: Option<Vec<CellRange>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ExcelSheetImage>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CellRange {
    pub s: CellCoord,
    pub e: CellCoord,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CellCoord {
    pub r: usize,
    pub c: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExcelSheetImage {
    pub row: usize,
    pub col: usize,
    pub src: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn history_target_requires_content_type() {
        assert!(serde_json::from_value::<PreviewHistoryTargetDto>(json!({"file_path": "/a.md"})).is_err());
    }

    #[test]
    fn history_target_roundtrips_optional_fields() {
        let target: PreviewHistoryTargetDto = serde_json::from_value(json!({
            "content_type": "markdown",
            "file_path": "/a.md",
            "workspace": "/workspace",
            "conversation_id": "conversation-1"
        }))
        .unwrap();
        assert_eq!(target.file_path.as_deref(), Some("/a.md"));
        assert_eq!(target.workspace.as_deref(), Some("/workspace"));
        assert_eq!(target.conversation_id.as_deref(), Some("conversation-1"));
    }

    #[test]
    fn conversion_target_accepts_supported_formats() {
        assert_eq!(
            serde_json::from_value::<ConversionTarget>(json!("markdown")).unwrap(),
            ConversionTarget::Markdown
        );
        assert_eq!(
            serde_json::from_value::<ConversionTarget>(json!("excel-json")).unwrap(),
            ConversionTarget::ExcelJson
        );
    }

    #[test]
    fn conversion_target_rejects_removed_formats() {
        assert!(serde_json::from_value::<ConversionTarget>(json!("ppt-json")).is_err());
    }

    #[test]
    fn conversion_result_omits_empty_payloads() {
        let result = ConversionResultDto {
            success: false,
            data: None,
            error: None,
        };
        let value = serde_json::to_value(result).unwrap();
        assert!(value.get("data").is_none());
        assert!(value.get("error").is_none());
    }
}
