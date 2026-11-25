//! 对应上游 `src/core/tools/edit.ts`：一次改一处或多处，按原文精确匹配（不行再模糊匹配）。

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Map, Value};
use xjpcode_agent::{AgentTool, AgentToolResult, AgentToolUpdateCallback, ToolError};
use xjpcode_ai::types::ConstrainedSamplingStrict;
use xjpcode_ai::{
    AbortSignal, ConstrainedSampling, ConstrainedSamplingConfig, JsonObject, TextContent, Tool,
    ToolResultContentBlock,
};

use crate::core::tools::edit_diff::{
    apply_edits_to_normalized_content, describe_access_error, describe_io_error, generate_edit_diff,
    normalize_to_lf, Edit,
};
use crate::core::tools::file_mutation_queue::with_file_mutation_queue;
use crate::core::tools::path_utils::resolve_to_cwd;
use crate::core::tools::ToolSystemPromptContribution;
use crate::utils::text::split_bom;

/// pi: `editToolSystemPromptContribution`
pub fn edit_tool_system_prompt_contribution() -> ToolSystemPromptContribution {
    ToolSystemPromptContribution {
        snippet:
            "Make precise file edits with exact text replacement, including multiple disjoint edits in one call",
        guidelines: &[
            "Use edit for targeted changes. Batch changes to one file in a single call.",
            "Each edits[].oldText must exactly and uniquely match the original file. Keep matches short and non-overlapping; merge nearby changes without including large unchanged regions.",
        ],
    }
}

pub type EditReadFileFn =
    Arc<dyn Fn(PathBuf) -> xjpcode_ai::BoxFuture<'static, std::io::Result<Vec<u8>>> + Send + Sync>;
pub type EditWriteFileFn = Arc<
    dyn Fn(PathBuf, String) -> xjpcode_ai::BoxFuture<'static, std::io::Result<()>> + Send + Sync,
>;
pub type EditAccessFn =
    Arc<dyn Fn(PathBuf) -> xjpcode_ai::BoxFuture<'static, std::io::Result<()>> + Send + Sync>;

/// pi: `EditOperations`
#[derive(Clone)]
pub struct EditOperations {
    pub read_file: EditReadFileFn,
    pub write_file: EditWriteFileFn,
    pub access: EditAccessFn,
}

impl Default for EditOperations {
    fn default() -> Self {
        Self {
            read_file: Arc::new(|path| Box::pin(async move { tokio::fs::read(path).await })),
            write_file: Arc::new(|path, content| {
                Box::pin(async move { tokio::fs::write(path, content).await })
            }),
            access: Arc::new(|path| {
                Box::pin(async move {
                    // 上游 `access(R_OK | W_OK)`：能读能写才算过。
                    tokio::task::spawn_blocking(move || {
                        std::fs::OpenOptions::new()
                            .read(true)
                            .write(true)
                            .open(path)
                            .map(|_| ())
                    })
                    .await
                    .map_err(|error| std::io::Error::other(error.to_string()))?
                })
            }),
        }
    }
}

/// pi: `EditToolOptions`
#[derive(Clone, Default)]
pub struct EditToolOptions {
    pub operations: Option<EditOperations>,
}

fn is_single_edit_input(value: &Value) -> bool {
    value
        .as_object()
        .is_some_and(|object| object.get("oldText").is_some_and(Value::is_string) && object.get("newText").is_some_and(Value::is_string))
}

/// pi: `prepareEditArguments`——模型偶尔把 edits 写成字符串或单个对象，也可能把 oldText/newText 放在
/// 顶层。这里在参数校验之前把它们折成 `edits` 数组。
pub fn prepare_edit_arguments(input: &JsonObject) -> JsonObject {
    let mut args = input.clone();

    if let Some(Value::String(edits_text)) = args.get("edits") {
        if let Ok(parsed) = serde_json::from_str::<Value>(edits_text) {
            if parsed.is_array() {
                args.insert("edits".to_string(), parsed);
            } else if is_single_edit_input(&parsed) {
                args.insert("edits".to_string(), Value::Array(vec![parsed]));
            }
        }
    } else if args.get("edits").is_some_and(is_single_edit_input) {
        let edit = args.get("edits").cloned().expect("edits");
        args.insert("edits".to_string(), Value::Array(vec![edit]));
    }

    let legacy_old = args.get("oldText").and_then(Value::as_str).map(str::to_string);
    let legacy_new = args.get("newText").and_then(Value::as_str).map(str::to_string);
    let (Some(old_text), Some(new_text)) = (legacy_old, legacy_new) else {
        return args;
    };

    let mut edits: Vec<Value> = match args.get("edits") {
        Some(Value::Array(edits)) => edits.clone(),
        _ => Vec::new(),
    };
    edits.push(json!({ "oldText": old_text, "newText": new_text }));
    args.insert("edits".to_string(), Value::Array(edits));
    args.remove("oldText");
    args.remove("newText");
    args
}

fn validate_edit_input(input: &JsonObject) -> Result<(String, Vec<Edit>), ToolError> {
    let path = input
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::from("edit requires a path"))?
        .to_string();
    let edits = input.get("edits").and_then(Value::as_array);
    let Some(edits) = edits.filter(|edits| !edits.is_empty()) else {
        return Err(ToolError::from(
            "Edit tool input is invalid. edits must contain at least one replacement.",
        ));
    };
    let mut parsed = Vec::new();
    for edit in edits {
        let Some(edit) = edit.as_object() else {
            return Err(ToolError::from(
                "Edit tool input is invalid. edits must contain at least one replacement.",
            ));
        };
        let old_text = edit
            .get("oldText")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::from("edit requires oldText for every edit"))?;
        let new_text = edit
            .get("newText")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::from("edit requires newText for every edit"))?;
        parsed.push(Edit {
            old_text: old_text.to_string(),
            new_text: new_text.to_string(),
        });
    }
    Ok((path, parsed))
}

/// pi: `createEditTool`
pub struct EditTool {
    cwd: PathBuf,
    operations: EditOperations,
}

impl EditTool {
    /// 上游工具定义上的 `promptSnippet` / `promptGuidelines`。
    pub fn system_prompt_contribution(&self) -> ToolSystemPromptContribution {
        edit_tool_system_prompt_contribution()
    }

    pub fn new(cwd: impl Into<PathBuf>, options: EditToolOptions) -> Self {
        Self {
            cwd: cwd.into(),
            operations: options.operations.unwrap_or_default(),
        }
    }
}

/// pi: `createEditTool`
pub fn create_edit_tool(cwd: impl Into<PathBuf>, options: EditToolOptions) -> EditTool {
    EditTool::new(cwd, options)
}

impl AgentTool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn label(&self) -> &str {
        "edit"
    }

    fn declaration(&self) -> Tool {
        Tool {
            name: self.name().to_string(),
            description: "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to the file to edit (relative or absolute)" },
                    "edits": {
                        "type": "array",
                        "description": "One or more targeted replacements. Each edit is matched against the original file, not incrementally. Do not include overlapping or nested edits. If two changes touch the same block or nearby lines, merge them into one edit instead.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "oldText": { "type": "string", "description": "Exact text for one targeted replacement. It must be unique in the original file and must not overlap with any other edits[].oldText in the same call." },
                                "newText": { "type": "string", "description": "Replacement text for this targeted edit." }
                            },
                            "required": ["oldText", "newText"]
                        }
                    }
                },
                "required": ["path", "edits"]
            }),
            constrained_sampling: Some(ConstrainedSampling::Config(
                ConstrainedSamplingConfig::JsonSchema {
                    strict: ConstrainedSamplingStrict::Prefer,
                },
            )),
        }
    }

    fn prepare_arguments(&self, args: &JsonObject) -> Result<JsonObject, ToolError> {
        Ok(prepare_edit_arguments(args))
    }

    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        params: &'a JsonObject,
        signal: Option<&'a AbortSignal>,
        _on_update: Option<&'a AgentToolUpdateCallback>,
    ) -> xjpcode_ai::BoxFuture<'a, Result<AgentToolResult, ToolError>> {
        Box::pin(async move {
            let (path, edits) = validate_edit_input(params)?;
            let absolute_path = resolve_to_cwd(&path, &self.cwd.to_string_lossy());
            let operations = self.operations.clone();
            let edit_count = edits.len();
            let queue_path = absolute_path.clone();

            with_file_mutation_queue(&queue_path, move || async move {
                // 不在 abort 监听里直接 reject：那会让队列在文件操作还没落地时就放行（上游同义）。
                let throw_if_aborted = || {
                    if signal.is_some_and(AbortSignal::aborted) {
                        Err(ToolError::from("Operation aborted"))
                    } else {
                        Ok(())
                    }
                };

                throw_if_aborted()?;
                if let Err(error) = (operations.access)(absolute_path.clone()).await {
                    throw_if_aborted()?;
                    return Err(ToolError::from(format!(
                        "Could not edit file: {path}. {}.",
                        describe_access_error(&error)
                    )));
                }
                throw_if_aborted()?;

                let buffer = (operations.read_file)(absolute_path.clone())
                    .await
                    .map_err(|error| ToolError::from(describe_io_error(&error)))?;
                throw_if_aborted()?;
                let raw_content = String::from_utf8_lossy(&buffer).into_owned();

                let (bom, content) = split_bom(&raw_content);
                let normalized_content = normalize_to_lf(content);
                let applied =
                    apply_edits_to_normalized_content(&normalized_content, &edits, &path)?;
                // 先算 diff 再写：diff 出错时文件保持原样。
                let edit_diff = generate_edit_diff(
                    &path,
                    &applied.base_content,
                    &applied.new_content,
                    &applied.replacements,
                    4,
                );
                throw_if_aborted()?;

                let final_content = format!(
                    "{bom}{}",
                    if content == normalized_content {
                        applied.new_content.clone()
                    } else {
                        crate::core::tools::edit_diff::apply_replacements_to_original(
                            content,
                            &applied.replacements
                        )
                    }
                );
                (operations.write_file)(absolute_path.clone(), final_content)
                    .await
                    .map_err(|error| ToolError::from(describe_io_error(&error)))?;
                throw_if_aborted()?;

                let mut details = Map::new();
                details.insert("diff".to_string(), Value::String(edit_diff.diff));
                details.insert("patch".to_string(), Value::String(edit_diff.patch));
                if let Some(first_changed_line) = edit_diff.first_changed_line {
                    details.insert(
                        "firstChangedLine".to_string(),
                        Value::Number(serde_json::Number::from(first_changed_line)),
                    );
                }

                Ok(AgentToolResult {
                    content: vec![ToolResultContentBlock::Text(TextContent::new(format!(
                        "Successfully replaced {edit_count} block(s) in {path}."
                    )))],
                    details: Some(Value::Object(details)),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                })
            })
            .await
        })
    }
}
