//! 对应上游 `src/core/compaction/index.ts`：压缩与摘要。

pub mod compaction;
pub mod utils;

pub use compaction::{
    calculate_context_tokens, combine_usage, compact, compaction_result_with_summary,
    estimate_context_tokens, estimate_projected_context_tokens, estimate_tokens,
    find_cut_point, find_turn_start_index, get_last_assistant_usage, get_summarization_failure,
    prepare_compaction, should_compact, CompactionPreparation, CompactionResult,
    CompactionSettings, ContextUsageEstimate, CutPointResult, DEFAULT_COMPACTION_SETTINGS,
};
pub use utils::{
    compute_file_lists, create_file_ops, extract_file_ops_from_message, format_file_operations,
    serialize_conversation, FileOperations, SUMMARIZATION_SYSTEM_PROMPT,
};
