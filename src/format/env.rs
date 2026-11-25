use super::cwd::display_cwd;

/// 注入给模型的运行环境分节(唯一出处):system prompt 与 round_lisp librarian payload 共用。
/// 认知核心原则:模型对本地状态的唯一感知来源——无此则 "." 等相对路径在上下文中失去锚点。
pub fn prompt_env_section() -> String {
    format!(
        "--- Environment ---\ncwd: {}\nplatform: {}",
        display_cwd(),
        std::env::consts::OS
    )
}
