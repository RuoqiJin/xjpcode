use crate::ui::App;

pub fn text(app: &App) -> String {
    let mut parts = Vec::new();

    if !app.session_id.is_empty() {
        // 编号长了会把右边的 context 挤出去；点状态栏复制的是完整编号。
        let short: String = app.session_id.chars().take(8).collect();
        parts.push(format!("session: {short}"));
    }

    if app.context_window > 0 && app.context_prompt_tokens > 0 {
        let tokens = app.context_prompt_tokens as u64;
        let limit = app.context_window;
        let pct = tokens as f64 / limit as f64 * 100.0;
        let pct_label = if pct < 1.0 {
            "<1%".to_string()
        } else {
            format!("{pct:.0}%")
        };
        parts.push(format!(
            "context: {} ({}/{})",
            pct_label,
            crate::format::numbers::humanize_count(tokens),
            crate::format::numbers::humanize_count(limit)
        ));
    }

    parts.join(" | ")
}
