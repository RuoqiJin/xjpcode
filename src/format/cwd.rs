use std::path::PathBuf;

/// 当前工作目录的展示形式:能定位到 HOME 时用 ~ 折叠前缀,全项目唯一出处。
pub fn display_cwd() -> String {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let Some(home) = dirs::home_dir() else {
        return cwd.display().to_string();
    };
    match cwd.strip_prefix(&home) {
        Ok(rel) if rel.as_os_str().is_empty() => "~".to_string(),
        Ok(rel) => format!("~/{}", rel.display()),
        Err(_) => cwd.display().to_string(),
    }
}
