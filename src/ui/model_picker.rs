use super::picker::{PickerItem, PickerKind, PickerState};
use xjpcode_ai::Model;

/// 构建模型选择菜单（/model 无参数时打开；选中项定位到当前模型）。
pub fn build_model_picker(
    models: &[Model],
    current_provider: &str,
    current_id: &str,
) -> PickerState {
    let items = models
        .iter()
        .map(|model| PickerItem {
            id: format!("{}/{}", model.provider, model.id),
            label: format!("{}/{}", model.provider, model.id),
            detail: format!(
                "{} · 窗口 {}",
                model.name,
                crate::format::numbers::humanize_count(model.context_window)
            ),
            active: model.provider == current_provider && model.id == current_id,
        })
        .collect();
    let index = models
        .iter()
        .position(|model| model.provider == current_provider && model.id == current_id)
        .unwrap_or(0);
    PickerState {
        kind: PickerKind::Model,
        title: "选择模型 (↑/↓ 切换，Enter 确认)",
        items,
        index,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xjpcode_ai::providers::model_schema::ModelCost;
    use xjpcode_ai::providers::model_schema::ModelInputModality;

    fn model(provider: &str, id: &str) -> Model {
        Model {
            id: id.to_string(),
            name: id.to_string(),
            api: "openai-completions".to_string(),
            provider: provider.to_string(),
            base_url: "http://localhost".to_string(),
            input: vec![ModelInputModality::Text],
            input_limits: None,
            cost: ModelCost::default(),
            headers: None,
            r#type: None,
            reasoning: false,
            thinking_level_map: None,
            prompt_cache: None,
            context_window: 1_048_576,
            max_tokens: 8_192,
            sampling_params: None,
            sampling_params_by_thinking_level: None,
            compat: None,
        }
    }

    #[test]
    fn picker_marks_the_current_model() {
        let models = vec![
            model("deepseek", "deepseek-flash"),
            model("openai-codex", "gpt"),
        ];
        let picker = build_model_picker(&models, "openai-codex", "gpt");
        assert_eq!(picker.index, 1);
        assert!(picker.items[1].active);
        assert_eq!(picker.items[0].id, "deepseek/deepseek-flash");
        assert!(picker.items[0].detail.contains("1M"));
    }
}
