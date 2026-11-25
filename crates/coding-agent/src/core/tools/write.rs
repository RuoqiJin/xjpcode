//! 对应上游 `src/core/tools/write.ts`：写文件（自动建父目录）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::json;
use xjpcode_agent::{AgentTool, AgentToolResult, AgentToolUpdateCallback, ToolError};
use xjpcode_ai::types::ConstrainedSamplingStrict;
use xjpcode_ai::{
    AbortSignal, ConstrainedSampling, ConstrainedSamplingConfig, JsonObject, TextContent, Tool,
    ToolResultContentBlock,
};

use crate::core::tools::edit_diff::describe_io_error;
use crate::core::tools::file_mutation_queue::with_file_mutation_queue;
use crate::core::tools::path_utils::resolve_to_cwd;
use crate::core::tools::ToolSystemPromptContribution;

/// pi: `writeToolSystemPromptContribution`
pub fn write_tool_system_prompt_contribution() -> ToolSystemPromptContribution {
    ToolSystemPromptContribution {
        snippet: "Create or overwrite files",
        guidelines: &["Use write only for new files or complete rewrites."],
    }
}

pub type WriteFileFn = Arc<
    dyn Fn(PathBuf, String) -> xjpcode_ai::BoxFuture<'static, std::io::Result<()>> + Send + Sync,
>;
pub type WriteMkdirFn =
    Arc<dyn Fn(PathBuf) -> xjpcode_ai::BoxFuture<'static, std::io::Result<()>> + Send + Sync>;

/// pi: `WriteOperations`
#[derive(Clone)]
pub struct WriteOperations {
    pub write_file: WriteFileFn,
    pub mkdir: WriteMkdirFn,
}

impl Default for WriteOperations {
    fn default() -> Self {
        Self {
            write_file: Arc::new(|path, content| {
                Box::pin(async move { tokio::fs::write(path, content).await })
            }),
            mkdir: Arc::new(|dir| {
                Box::pin(async move { tokio::fs::create_dir_all(dir).await })
            }),
        }
    }
}

/// pi: `WriteToolOptions`
#[derive(Clone, Default)]
pub struct WriteToolOptions {
    pub operations: Option<WriteOperations>,
}

/// pi: `createWriteTool`
pub struct WriteTool {
    cwd: PathBuf,
    operations: WriteOperations,
}

impl WriteTool {
    /// 上游工具定义上的 `promptSnippet` / `promptGuidelines`。
    pub fn system_prompt_contribution(&self) -> ToolSystemPromptContribution {
        write_tool_system_prompt_contribution()
    }

    pub fn new(cwd: impl Into<PathBuf>, options: WriteToolOptions) -> Self {
        Self {
            cwd: cwd.into(),
            operations: options.operations.unwrap_or_default(),
        }
    }
}

/// pi: `createWriteTool`
pub fn create_write_tool(cwd: impl Into<PathBuf>, options: WriteToolOptions) -> WriteTool {
    WriteTool::new(cwd, options)
}

impl AgentTool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn label(&self) -> &str {
        "write"
    }

    fn declaration(&self) -> Tool {
        Tool {
            name: self.name().to_string(),
            description: "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to the file to write (relative or absolute)" },
                    "content": { "type": "string", "description": "Content to write to the file" }
                },
                "required": ["path", "content"]
            }),
            constrained_sampling: Some(ConstrainedSampling::Config(
                ConstrainedSamplingConfig::JsonSchema {
                    strict: ConstrainedSamplingStrict::Prefer,
                },
            )),
        }
    }

    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        params: &'a JsonObject,
        signal: Option<&'a AbortSignal>,
        _on_update: Option<&'a AgentToolUpdateCallback>,
    ) -> xjpcode_ai::BoxFuture<'a, Result<AgentToolResult, ToolError>> {
        Box::pin(async move {
            let path = params
                .get("path")
                .and_then(|value| value.as_str())
                .ok_or_else(|| ToolError::from("write requires a path"))?;
            let content = params
                .get("content")
                .and_then(|value| value.as_str())
                .ok_or_else(|| ToolError::from("write requires content"))?;
            let absolute_path = resolve_to_cwd(path, &self.cwd.to_string_lossy());
            let dir = absolute_path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));

            let operations = self.operations.clone();
            let path = path.to_string();
            let queue_path = absolute_path.clone();
            with_file_mutation_queue(&queue_path, move || async move {
                let throw_if_aborted = || {
                    if signal.is_some_and(AbortSignal::aborted) {
                        Err(ToolError::from("Operation aborted"))
                    } else {
                        Ok(())
                    }
                };
                throw_if_aborted()?;
                (operations.mkdir)(dir)
                    .await
                    .map_err(|error| ToolError::from(describe_io_error(&error)))?;
                throw_if_aborted()?;
                (operations.write_file)(absolute_path.clone(), content.to_string())
                    .await
                    .map_err(|error| ToolError::from(describe_io_error(&error)))?;
                throw_if_aborted()?;
                Ok(AgentToolResult {
                    content: vec![ToolResultContentBlock::Text(TextContent::new(format!(
                        "Successfully wrote to {path}"
                    )))],
                    details: None,
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
