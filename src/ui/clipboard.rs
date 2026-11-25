use std::io::Write;

/// 复制到系统剪贴板：macOS 走 `pbcopy`，其它平台（以及 `pbcopy` 起不来时）走 OSC 52。
pub fn copy_to_clipboard(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    #[cfg(target_os = "macos")]
    if copy_with_pbcopy(text) {
        return true;
    }
    write_osc52(text)
}

#[cfg(target_os = "macos")]
fn copy_with_pbcopy(text: &str) -> bool {
    use std::process::{Command, Stdio};

    let Ok(mut child) = Command::new("pbcopy").stdin(Stdio::piped()).spawn() else {
        return false;
    };
    let Some(mut stdin) = child.stdin.take() else {
        return false;
    };
    if stdin.write_all(text.as_bytes()).is_err() {
        return false;
    }
    // 必须在 wait 前关掉 stdin，给 pbcopy 发 EOF。
    drop(stdin);
    child.wait().map(|status| status.success()).unwrap_or(false)
}

fn write_osc52(text: &str) -> bool {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    let sequence = format!("\x1b]52;c;{encoded}\x07");
    let mut stdout = std::io::stdout();
    stdout.write_all(sequence.as_bytes()).is_ok() && stdout.flush().is_ok()
}
