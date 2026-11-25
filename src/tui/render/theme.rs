use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::BorderType;

pub struct Theme;

impl Theme {
    // 边框类型
    pub const BORDER_TYPE: BorderType = BorderType::Rounded;

    // 颜色 Token 定义 (Catppuccin / Nord 调色板风格)
    pub const COLOR_BORDER: Color = Color::Rgb(76, 86, 106); // 暗冷灰边框
    pub const COLOR_BORDER_ACTIVE: Color = Color::Rgb(136, 192, 208); // 柔和冰蓝高亮

    pub const COLOR_USER: Color = Color::Rgb(163, 190, 140); // 柔和薄荷绿
    pub const COLOR_AI: Color = Color::Rgb(122, 162, 247); // 莫兰迪雾蓝
    pub const COLOR_PAYLOAD: Color = Color::Rgb(105, 115, 133); // 幽静暗紫灰

    pub const COLOR_TEXT_MAIN: Color = Color::Rgb(216, 222, 233); // 柔和暖白
    pub const COLOR_TEXT_DIM: Color = Color::Rgb(129, 140, 160); // 沉静暗灰
    pub const COLOR_HELP_KEY: Color = Color::Rgb(235, 203, 139); // 快捷键高亮金
    pub const COLOR_ERROR: Color = Color::Rgb(191, 97, 106); // 柔和警告红 (Nord 赤红)

    pub const COLOR_SELECTION_BG: Color = Color::Rgb(67, 76, 94); // 划选高亮深底色
    pub const COLOR_SELECTION_FG: Color = Color::Rgb(236, 239, 244); // 划选文字高亮白

    pub const COLOR_BADGE_TEXT: Color = Color::Rgb(30, 30, 46); // 徽章底色上的深色字（YOU 徽章 / SystemNotice 徽章共用）

    // 常用 Style 预设
    pub fn style_header_title() -> Style {
        Style::default()
            .fg(Self::COLOR_AI)
            .add_modifier(Modifier::BOLD)
    }

    pub fn style_user_badge() -> Style {
        Style::default()
            .bg(Self::COLOR_AI)
            .fg(Self::COLOR_BADGE_TEXT)
            .add_modifier(Modifier::BOLD)
    }
}
