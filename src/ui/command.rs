#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    Model,
    Skill,
    Effort,
    Thinking,
    Compact,
    Clear,
    Resume,
    Help,
    Exit,
    Login,
    Logout,
    New,
    Session,
    Copy,
    Mouse,
    Tools,
}

pub struct SlashCommand {
    pub kind: CommandKind,
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub description: &'static str,
}

pub const ALL_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        kind: CommandKind::Model,
        name: "/model",
        aliases: &["/use", "/profile"],
        description: "选择模型（无参数时弹出菜单；也可 /model <名称|序号>）",
    },
    SlashCommand {
        kind: CommandKind::Effort,
        name: "/effort",
        aliases: &["/think"],
        description: "选择思考档位（无参数时弹出菜单；也可 /effort <档位>）",
    },
    SlashCommand {
        kind: CommandKind::Thinking,
        name: "/thinking",
        aliases: &[],
        description: "选择思考档位（与 /effort 相同）",
    },
    SlashCommand {
        kind: CommandKind::Compact,
        name: "/compact",
        aliases: &["/compress"],
        description: "压缩历史上下文，节省 Token 消耗",
    },
    SlashCommand {
        kind: CommandKind::Clear,
        name: "/clear",
        aliases: &[],
        description: "清空屏幕并开一场新会话",
    },
    SlashCommand {
        kind: CommandKind::Resume,
        name: "/resume",
        aliases: &["/history", "/load"],
        description: "恢复历史会话（无参数时弹出菜单；也可 /resume <编号|文件>）",
    },
    SlashCommand {
        kind: CommandKind::Skill,
        name: "/skill",
        aliases: &["/skills"],
        description: "查看技能（/skill 列出；/skill <名称> [话] 带上技能正文发出去）",
    },
    SlashCommand {
        kind: CommandKind::Login,
        name: "/login",
        aliases: &[],
        description: "登录 DeepSeek（钥匙）或 GPT 订阅（浏览器）",
    },
    SlashCommand {
        kind: CommandKind::Logout,
        name: "/logout",
        aliases: &[],
        description: "删除已保存的钥匙或登录",
    },
    SlashCommand {
        kind: CommandKind::New,
        name: "/new",
        aliases: &[],
        description: "开一场新会话",
    },
    SlashCommand {
        kind: CommandKind::Session,
        name: "/session",
        aliases: &[],
        description: "显示本场会话的编号和文件",
    },
    SlashCommand {
        kind: CommandKind::Copy,
        name: "/copy",
        aliases: &[],
        description: "复制最近一条回答（/copy N 复制往前第 N 条）",
    },
    SlashCommand {
        kind: CommandKind::Mouse,
        name: "/mouse",
        aliases: &[],
        description: "开关鼠标捕获（/mouse on|off|status）",
    },
    SlashCommand {
        kind: CommandKind::Tools,
        name: "/tools",
        aliases: &[],
        description: "列出可用的工具",
    },
    SlashCommand {
        kind: CommandKind::Help,
        name: "/help",
        aliases: &[],
        description: "查看帮助说明",
    },
    SlashCommand {
        kind: CommandKind::Exit,
        name: "/exit",
        aliases: &["/quit", "exit", "quit"],
        description: "退出程序",
    },
];

/// 取斜杠命令的第一个参数（命令词之后的第一个空白分隔词；无参时 None）。
pub fn first_arg(prompt: &str) -> Option<&str> {
    prompt.split_whitespace().nth(1)
}

/// 命令词之后的全部参数（保留中间空格；无参数时为空串）。
pub fn args_after_first(prompt: &str) -> &str {
    let trimmed = prompt.trim_start();
    match trimmed.find(char::is_whitespace) {
        Some(index) => trimmed[index..].trim(),
        None => "",
    }
}

/// 补全过滤：匹配 name 或以 `/` 开头的 alias 前缀（弹窗数据源）。
pub fn filter_commands(input: &str) -> Vec<&'static SlashCommand> {
    if !input.starts_with('/') {
        return Vec::new();
    }
    let trimmed = input.trim();
    let token = trimmed.split_whitespace().next().unwrap_or(trimmed);
    if ALL_COMMANDS
        .iter()
        .any(|cmd| cmd.name == token || cmd.aliases.contains(&token))
    {
        return Vec::new();
    }
    ALL_COMMANDS
        .iter()
        .filter(|cmd| {
            cmd.name.starts_with(token) || cmd.aliases.iter().any(|alias| alias.starts_with(token))
        })
        .collect()
}

/// 把用户输入解析为规范命令（分发数据源）：按空格分割首个词匹配 name 或 alias。
pub fn resolve(input: &str) -> Option<&'static SlashCommand> {
    let token = input.split_whitespace().next().unwrap_or(input);
    ALL_COMMANDS
        .iter()
        .find(|cmd| cmd.name == token || cmd.aliases.contains(&token))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xj_and_xjpcode_commands_have_their_aliases() {
        assert_eq!(resolve("/use").unwrap().kind, CommandKind::Model);
        assert_eq!(resolve("/profile").unwrap().kind, CommandKind::Model);
        assert_eq!(resolve("/think").unwrap().kind, CommandKind::Effort);
        assert_eq!(resolve("/compress").unwrap().kind, CommandKind::Compact);
        assert_eq!(resolve("/history").unwrap().kind, CommandKind::Resume);
        assert_eq!(resolve("/load").unwrap().kind, CommandKind::Resume);
        assert_eq!(resolve("/skills").unwrap().kind, CommandKind::Skill);
        assert_eq!(resolve("exit").unwrap().kind, CommandKind::Exit);
        assert_eq!(resolve("quit").unwrap().kind, CommandKind::Exit);
        assert_eq!(resolve("/quit").unwrap().kind, CommandKind::Exit);
    }

    #[test]
    fn exact_commands_close_the_completion_popup() {
        assert!(filter_commands("/model").is_empty());
        assert!(filter_commands("/use").is_empty());
        assert!(!filter_commands("/mo").is_empty());
        assert!(filter_commands("not a command").is_empty());
    }

    #[test]
    fn args_keep_everything_after_the_command_word() {
        assert_eq!(first_arg("/skill demo 后面 的话"), Some("demo"));
        assert_eq!(args_after_first("/skill demo 后面 的话"), "demo 后面 的话");
        assert_eq!(args_after_first("/skill"), "");
    }
}
