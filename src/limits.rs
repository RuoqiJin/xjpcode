//! 界面自己的预算。
//!
//! 这些数字只管显示和输入框（保留多少条消息、工具结果截到多宽、草稿最多多大），不限制模型能
//! 跑多少轮、调几次工具。内核那边照 pi 没有这类上限；这里单独放，免得和内核混在一起。

/// 界面上最多保留多少条消息（更早的从显示里丢掉，会话文件里仍有）。
pub const DISPLAY_MESSAGES_DEFAULT: usize = 10_000;
/// 每帧最多重画多少条消息（虚拟滚动的回溯窗口）。
pub const DISPLAY_RENDER_MESSAGES_DEFAULT: usize = 80;
/// 最近多少条工具结果保留全文，更早的换成一行摘要。
pub const DISPLAY_FULL_TOOL_RESULTS_DEFAULT: usize = 20;
/// 工具结果每行最多多少显示列。
pub const DISPLAY_TOOL_RESULT_COLUMNS_DEFAULT: usize = 200;
/// 工具结果预处理后最多多少字节。
pub const DISPLAY_TOOL_RESULT_BYTES_DEFAULT: usize = 100_000;
/// 输入历史最多保留多少条。
pub const INPUT_HISTORY_ENTRIES_DEFAULT: usize = 1_000;
/// 输入历史文件最多读多少字节（只读最近的这一段）。
pub const INPUT_HISTORY_BYTES_DEFAULT: usize = 512 * 1024;
/// 一条草稿最多多少字节。
pub const USER_INPUT_BYTES_PER_TURN_DEFAULT: usize = 1024 * 1024;
/// 输入框上方最多显示几行排队的消息。
pub const PENDING_QUEUE_ROWS_DEFAULT: usize = 4;
