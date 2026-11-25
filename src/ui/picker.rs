use super::selection_cycle::{cycle_next, cycle_prev};
use super::state::App;

/// 模态选择菜单的种类（Enter 确认时的分发依据）。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PickerKind {
    Model,
    Effort,
    Resume,
    Skill,
    Login,
    Logout,
}

/// 菜单项（id 为确认时的业务键，label/detail/active 仅用于展示）。
pub struct PickerItem {
    pub id: String,
    pub label: String,
    pub detail: String,
    pub active: bool,
}

/// 模态选择菜单的通用状态（列表 + 选中索引；打开即模态，↑/↓ 循环，Esc 取消）。
pub struct PickerState {
    pub kind: PickerKind,
    pub title: &'static str,
    pub items: Vec<PickerItem>,
    pub index: usize,
}

impl App {
    pub fn open_picker(&mut self, picker: PickerState) {
        self.picker = Some(picker);
        self.popup_dismissed = true;
    }

    pub fn close_picker(&mut self) {
        self.picker = None;
    }

    pub fn has_picker(&self) -> bool {
        self.picker.is_some()
    }

    pub fn picker_kind(&self) -> Option<PickerKind> {
        self.picker.as_ref().map(|picker| picker.kind)
    }

    pub fn picker_move_up(&mut self) {
        if let Some(picker) = self.picker.as_mut() {
            picker.index = cycle_prev(picker.index, picker.items.len());
        }
    }

    pub fn picker_move_down(&mut self) {
        if let Some(picker) = self.picker.as_mut() {
            picker.index = cycle_next(picker.index, picker.items.len());
        }
    }

    /// 当前选中项的业务键（Enter 确认时的唯一数据源）。
    pub fn selected_picker_item(&self) -> Option<&str> {
        let picker = self.picker.as_ref()?;
        picker.items.get(picker.index).map(|item| item.id.as_str())
    }
}
