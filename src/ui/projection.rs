//! 把 `AgentSessionEvent` 变成 xj 风格的卡片。
//!
//! 界面不自己决定会话生命周期：收到什么事件画什么。文本增量写进当前回答卡，
//! 工具段切断回答卡；重试、压缩、排队都如实显示。

use xjpcode_agent::{AgentMessage, AgentToolResult};
use xjpcode_ai::{AssistantContentBlock, ToolResultContentBlock};
use xjpcode_coding_agent::core::agent_session::{AgentSessionEvent, CompactionReason};

use super::spinner;
use super::state::App;

fn reason_name(reason: CompactionReason) -> &'static str {
    match reason {
        CompactionReason::Manual => "手动",
        CompactionReason::Threshold => "达到门槛",
        CompactionReason::Overflow => "上下文超限",
    }
}

fn pretty_arguments(arguments: &xjpcode_ai::JsonObject) -> String {
    serde_json::to_string_pretty(&serde_json::Value::Object(arguments.clone()))
        .unwrap_or_else(|_| "{}".to_string())
}

pub fn tool_result_text(result: &AgentToolResult) -> String {
    result
        .content
        .iter()
        .map(|block| match block {
            ToolResultContentBlock::Text(text) => text.text.clone(),
            ToolResultContentBlock::Image(_) => "[image]".to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn apply_assistant_end(app: &mut App, assistant: &xjpcode_ai::AssistantMessage) {
    let text = assistant
        .content
        .iter()
        .filter_map(|block| match block {
            AssistantContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let reasoning = assistant
        .content
        .iter()
        .filter_map(|block| match block {
            AssistantContentBlock::Thinking(thinking) if !thinking.thinking.trim().is_empty() => {
                Some(thinking.thinking.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    if !text.trim().is_empty() || !reasoning.trim().is_empty() {
        let id = app.ensure_assistant_block();
        if let Some(block) = app.blocks.iter_mut().find(|block| block.id == id) {
            if !text.is_empty() {
                block.content = text;
            }
            if !reasoning.is_empty() {
                block.reasoning = Some(reasoning);
            }
        }
    }

    match assistant.stop_reason {
        xjpcode_ai::StopReason::Error => {
            app.add_system_notice(
                "请求失败",
                format!(
                    "本轮请求未能完成：\n{}",
                    assistant
                        .error_message
                        .as_deref()
                        .unwrap_or("Unknown error")
                ),
            );
        }
        xjpcode_ai::StopReason::Aborted => {
            app.add_system_notice("已取消", "已取消本轮请求，已生成的内容保留在上下文中。");
        }
        xjpcode_ai::StopReason::Length => {
            app.add_system_notice("回答被截断", "模型在输出上限处停下，回答没有写完。");
        }
        _ => {}
    }
    app.cut_active_assistant_block();
}

pub fn apply(event: &AgentSessionEvent, app: &mut App) {
    match event {
        AgentSessionEvent::AgentStart => {
            if !app.spinner.is_processing {
                app.spinner.start(spinner::get_random_verb());
            }
        }
        AgentSessionEvent::AgentEnd { .. } | AgentSessionEvent::TurnStart => {}
        AgentSessionEvent::TurnEnd { message, .. } => {
            if let AgentMessage::Assistant(assistant) = message {
                if assistant.usage.total_tokens > 0 {
                    app.spinner
                        .set_usage(assistant.usage.input, assistant.usage.output);
                }
            }
        }
        AgentSessionEvent::MessageStart { message } => {
            if let AgentMessage::User(user) = message {
                let text = match &user.content {
                    xjpcode_ai::UserContent::Text(text) => text.clone(),
                    xjpcode_ai::UserContent::Blocks(blocks) => blocks
                        .iter()
                        .filter_map(|block| match block {
                            xjpcode_ai::UserContentBlock::Text(text) => Some(text.text.clone()),
                            xjpcode_ai::UserContentBlock::Image(_) => Some("[image]".to_string()),
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                };
                if !text.trim().is_empty() {
                    app.add_user_block(text);
                }
            } else if let AgentMessage::Assistant(_) = message {
                app.cut_active_assistant_block();
            }
        }
        AgentSessionEvent::MessageUpdate {
            assistant_message_event,
            ..
        } => match assistant_message_event {
            xjpcode_ai::AssistantMessageEvent::TextDelta { delta, .. } => {
                app.spinner.add_chars(delta.len());
                app.spinner.set_verb("正在回答...");
                app.append_content(delta);
            }
            xjpcode_ai::AssistantMessageEvent::ThinkingDelta { delta, .. } => {
                app.spinner.add_chars(delta.len());
                app.spinner.set_verb("思考中...");
                app.append_reasoning(delta);
            }
            _ => {}
        },
        AgentSessionEvent::MessageEnd { message } => {
            if let AgentMessage::Assistant(assistant) = message {
                apply_assistant_end(app, assistant);
            }
        }
        AgentSessionEvent::ToolExecutionStart {
            tool_call_id,
            tool_name,
            args,
        } => {
            app.cut_active_assistant_block();
            app.active_tool_start = Some(std::time::Instant::now());
            app.spinner.set_verb(format!("正在运行工具 {tool_name}..."));
            app.add_tool_call_block(tool_name, tool_call_id, &pretty_arguments(args));
        }
        AgentSessionEvent::ToolExecutionUpdate { .. } => {}
        AgentSessionEvent::ToolExecutionEnd {
            tool_call_id,
            tool_name,
            result,
            is_error,
            ..
        } => {
            app.cut_active_assistant_block();
            app.spinner.set_verb("正在处理工具结果...");
            let elapsed = app
                .active_tool_start
                .take()
                .map(|start| start.elapsed().as_secs_f32());
            let text = tool_result_text(result);
            if !app.merge_tool_result(tool_call_id, tool_name, &text, elapsed, *is_error) {
                app.add_tool_call_block(tool_name, tool_call_id, "");
                app.merge_tool_result(tool_call_id, tool_name, &text, elapsed, *is_error);
            }
        }
        AgentSessionEvent::QueueUpdate {
            steering,
            follow_up,
        } => app.set_pending_queue(steering.clone(), follow_up.clone()),
        AgentSessionEvent::AutoRetryStart {
            attempt,
            max_attempts,
            delay_ms,
            error_message,
        } => {
            app.spinner.start(format!(
                "重试 {attempt}/{max_attempts}（{:.1}s 后）",
                *delay_ms as f64 / 1000.0
            ));
            app.add_system_notice(
                "⟳ 重试",
                format!(
                    "第 {attempt}/{max_attempts} 次重试，{:.1} 秒后再发：{error_message}",
                    *delay_ms as f64 / 1000.0
                ),
            );
        }
        AgentSessionEvent::AutoRetryEnd {
            success,
            attempt,
            final_error,
        } => {
            if *success {
                app.add_system_notice("⟳ 重试成功", format!("第 {attempt} 次重试成功了。"));
            } else {
                app.add_system_notice(
                    "⟳ 重试失败",
                    format!(
                        "重试了 {attempt} 次仍未成功：{}",
                        final_error.as_deref().unwrap_or("Unknown error")
                    ),
                );
            }
        }
        AgentSessionEvent::CompactionStart { reason } => {
            if !app.spinner.is_processing {
                app.spinner.start("正在压缩上下文...");
            } else {
                app.spinner.set_verb("正在压缩上下文...");
            }
            app.add_system_notice(
                "压缩上下文",
                format!("开始压缩（{}）。", reason_name(*reason)),
            );
        }
        AgentSessionEvent::CompactionEnd {
            reason,
            result,
            aborted,
            will_retry,
            error_message,
        } => {
            if *aborted {
                app.add_system_notice(
                    "压缩上下文",
                    format!("压缩已取消（{}）。", reason_name(*reason)),
                );
            } else if let Some(result) = result {
                app.add_system_notice(
                    "压缩完成",
                    format!(
                        "压缩完成（{}）：{} → {} tokens{}",
                        reason_name(*reason),
                        result.tokens_before,
                        result.estimated_tokens_after.unwrap_or(0),
                        if *will_retry { "，随后重试" } else { "" }
                    ),
                );
            } else if let Some(error_message) = error_message {
                app.add_system_notice("压缩失败", error_message.clone());
            }
            if *aborted || !*will_retry {
                app.spinner.stop();
            }
        }
        AgentSessionEvent::SummarizationRetryScheduled {
            attempt,
            max_attempts,
            delay_ms,
            error_message,
        } => {
            app.add_system_notice(
                "⟳ 摘要重试",
                format!(
                    "第 {attempt}/{max_attempts} 次，{:.1}s 后：{error_message}",
                    *delay_ms as f64 / 1000.0
                ),
            );
        }
        AgentSessionEvent::SummarizationRetryAttemptStart => {
            app.spinner.set_verb("正在压缩上下文...");
        }
        AgentSessionEvent::SummarizationRetryFinished => {}
        AgentSessionEvent::AgentSettled { .. } => {
            app.spinner.stop();
            app.cut_active_assistant_block();
        }
    }
}

/// 给测试用：取出所有卡片的纯文本。
#[cfg(test)]
pub fn block_texts(app: &App) -> Vec<String> {
    app.blocks
        .iter()
        .map(|block| format!("{}|{}", block.title, block.content))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::BlockKind;
    use xjpcode_agent::AgentToolResult;
    use xjpcode_ai::{AssistantMessage, AssistantMessageEvent, AssistantRole, StopReason, Usage};

    fn assistant(content: Vec<AssistantContentBlock>, stop_reason: StopReason) -> AssistantMessage {
        AssistantMessage {
            role: AssistantRole::Assistant,
            content,
            api: "openai-completions".to_string(),
            provider: "deepseek".to_string(),
            model: "deepseek-flash".to_string(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            thinking_level: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason,
            deferred: None,
            error_message: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 0,
            duration_ms: None,
        }
    }

    #[test]
    fn text_and_thinking_deltas_share_one_card_until_a_tool_cuts_it() {
        let mut app = App::new();
        apply(
            &AgentSessionEvent::MessageUpdate {
                message: AgentMessage::Assistant(assistant(vec![], StopReason::Stop)),
                assistant_message_event: AssistantMessageEvent::ThinkingDelta {
                    content_index: 0,
                    delta: "想一下".to_string(),
                    partial: assistant(vec![], StopReason::Stop),
                },
            },
            &mut app,
        );
        apply(
            &AgentSessionEvent::MessageUpdate {
                message: AgentMessage::Assistant(assistant(vec![], StopReason::Stop)),
                assistant_message_event: AssistantMessageEvent::TextDelta {
                    content_index: 1,
                    delta: "回答".to_string(),
                    partial: assistant(vec![], StopReason::Stop),
                },
            },
            &mut app,
        );
        assert_eq!(app.blocks.len(), 1);
        assert_eq!(app.blocks[0].content, "回答");
        assert_eq!(app.blocks[0].reasoning.as_deref(), Some("想一下"));

        apply(
            &AgentSessionEvent::ToolExecutionStart {
                tool_call_id: "call_1".to_string(),
                tool_name: "read".to_string(),
                args: xjpcode_ai::JsonObject::new(),
            },
            &mut app,
        );
        assert_eq!(app.blocks.len(), 2);
        assert_eq!(app.blocks[1].kind, BlockKind::ToolCall);

        apply(
            &AgentSessionEvent::ToolExecutionEnd {
                tool_call_id: "call_1".to_string(),
                tool_name: "read".to_string(),
                result: AgentToolResult {
                    content: vec![ToolResultContentBlock::Text(xjpcode_ai::TextContent::new(
                        "ok",
                    ))],
                    details: None,
                    structured_content: None,
                    usage: None,
                    is_error: Some(false),
                    terminate: None,
                },
                is_error: false,
                duration_ms: Some(1),
            },
            &mut app,
        );
        assert!(app.blocks[1].content.contains("结果"));
        assert!(app.blocks[1].title.contains("(2字符"));
    }

    #[test]
    fn turn_errors_get_a_full_notice() {
        let mut app = App::new();
        let mut message = assistant(vec![], StopReason::Error);
        message.error_message = Some("第一行\n第二行".to_string());
        apply(
            &AgentSessionEvent::MessageEnd {
                message: AgentMessage::Assistant(message),
            },
            &mut app,
        );
        assert_eq!(app.blocks[0].kind, BlockKind::SystemNotice);
        assert!(app.blocks[0].content.contains("第二行"));
    }
}
