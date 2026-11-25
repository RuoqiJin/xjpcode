//! 对应上游 `packages/ai/src/utils/diagnostics.ts` 中被 `types.ts` 引用的部分。

use serde::{Deserialize, Serialize};

use crate::types::JsonObject;

/// pi: `DiagnosticErrorInfo.code` = `string | number`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DiagnosticCode {
    Text(String),
    Number(serde_json::Number),
}

/// pi: `DiagnosticErrorInfo`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticErrorInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<DiagnosticCode>,
}

/// pi: `AssistantMessageDiagnostic`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssistantMessageDiagnostic {
    /// pi 的字段名就是 `type`。
    #[serde(rename = "type")]
    pub r#type: String,
    pub timestamp: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<DiagnosticErrorInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonObject>,
}
