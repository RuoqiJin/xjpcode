use super::command::{filter_commands, SlashCommand};
use super::selection_cycle::{cycle_next, cycle_prev};
use super::state::App;

impl App {
    pub fn matching_commands(&self) -> Vec<&'static SlashCommand> {
        if self.popup_dismissed {
            Vec::new()
        } else {
            filter_commands(&self.input)
        }
    }

    /// 命令补全弹窗当前是否处于激活状态（输入以 / 开头且存在匹配命令）
    pub fn has_command_popup(&self) -> bool {
        !self.matching_commands().is_empty()
    }

    /// 关闭命令补全弹窗（Esc 单一裁决点调用,见 keyboard/router.rs）。
    pub fn dismiss_popup(&mut self) {
        self.popup_dismissed = true;
    }

    /// 编辑输入即复活弹窗：清除 Esc 关闭标记并把选中项归零
    /// （typing/char_input、换行插入等所有「输入内容变了」的入口统一走这里）。
    pub fn rearm_on_edit(&mut self) {
        self.popup_dismissed = false;
        self.popup_index = 0;
    }

    pub fn popup_move_up(&mut self) {
        let count = self.matching_commands().len();
        self.popup_index = cycle_prev(self.popup_index, count);
    }

    pub fn popup_move_down(&mut self) {
        let count = self.matching_commands().len();
        self.popup_index = cycle_next(self.popup_index, count);
    }

    pub fn selected_command(&self) -> Option<&'static SlashCommand> {
        let cmds = self.matching_commands();
        if cmds.is_empty() {
            None
        } else {
            let idx = self.popup_index.min(cmds.len() - 1);
            Some(cmds[idx])
        }
    }

    pub fn apply_selected_command(&mut self) -> bool {
        if let Some(cmd) = self.selected_command() {
            self.input = cmd.name.to_string();
            self.popup_index = 0;
            true
        } else {
            false
        }
    }
}
