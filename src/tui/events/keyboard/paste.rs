use crate::ui::App;

/// 处理 Bracketed Paste 事件：终端把整段剪贴板文本作为一个原子事件送达，
/// 其中的换行不会经过 Enter 键分发，因此天然不会触发中途提交。
/// 登录提示里也照样接受（钥匙、回调链接都能贴）。
pub fn handle_paste(text: &str, app: &mut App) {
    let normalized = normalize_pasted_text(text);
    if let Some(prompt) = app.login_prompt.as_mut() {
        prompt.input.push_str(&normalized);
        prompt.notice = None;
    } else {
        app.insert_text(&normalized);
    }
    app.rearm_on_edit();
}

pub fn normalize_pasted_text(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter(|ch| *ch == '\n' || *ch == '\t' || !ch.is_control())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::state::LoginPrompt;

    #[test]
    fn paste_normalizes_newlines_and_drops_controls() {
        assert_eq!(
            normalize_pasted_text("one\r\ntwo\rthree\0\u{7}四\t五"),
            "one\ntwo\nthree四\t五"
        );
    }

    #[test]
    fn paste_goes_into_the_login_prompt_when_it_is_open() {
        let mut app = App::new();
        app.login_prompt = Some(LoginPrompt {
            message: "key".to_string(),
            secret: true,
            input: String::new(),
            notice: Some("notice".to_string()),
        });
        handle_paste("sk-123", &mut app);
        let prompt = app.login_prompt.as_ref().unwrap();
        assert_eq!(prompt.input, "sk-123");
        assert!(prompt.notice.is_none());
        assert!(app.input.is_empty());
    }
}
