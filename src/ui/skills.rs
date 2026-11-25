use xjpcode_coding_agent::Skill;

fn scope_label(skill: &Skill) -> &'static str {
    match skill.source_info.source.as_str() {
        "user" => "全局",
        "project" => "项目",
        _ => "路径",
    }
}

/// `/skill` 无参数时的列表（xj 的排版：序号、名字、范围、说明、路径）。
pub fn list_text(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return "目前未检测到任何技能。".to_string();
    }
    skills
        .iter()
        .enumerate()
        .map(|(index, skill)| {
            format!(
                "{}. {} [{}]\n   {}\n   路径: {}",
                index + 1,
                skill.name,
                scope_label(skill),
                skill.description,
                skill.file_path
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub fn find<'a>(skills: &'a [Skill], name: &str) -> Option<&'a Skill> {
    skills.iter().find(|skill| skill.name == name)
}

/// pi: `_expandSkillCommand`——按 `skill.filePath` 读原文，去掉 frontmatter 后包进 `<skill>`。
pub fn skill_block(skill: &Skill) -> Result<String, String> {
    let content = std::fs::read_to_string(&skill.file_path)
        .map_err(|error| format!("读技能 {} 失败: {error}", skill.file_path))?;
    let body = xjpcode_coding_agent::core::skills::strip_frontmatter(&content);
    let body = body.trim();
    Ok(format!(
        "<skill name=\"{}\" location=\"{}\">\nReferences are relative to {}.\n\n{}\n</skill>",
        skill.name, skill.file_path, skill.base_dir, body
    ))
}

/// `/skill 名字 后面的话`：技能正文 + 后面的话，作为一句用户输入发出去。
pub fn expand_named(skills: &[Skill], name: &str, args: &str) -> Result<Option<String>, String> {
    let Some(skill) = find(skills, name) else {
        return Ok(None);
    };
    let block = skill_block(skill)?;
    Ok(Some(if args.trim().is_empty() {
        block
    } else {
        format!("{block}\n\n{}", args.trim())
    }))
}

/// 也认 pi 的 `/skill:名字 [话]`；名字不认识时原样返回。
pub fn expand_skill_command(text: &str, skills: &[Skill]) -> String {
    let Some(rest) = text.strip_prefix("/skill:") else {
        return text.to_string();
    };
    let (name, args) = match rest.find(char::is_whitespace) {
        Some(index) => (&rest[..index], rest[index..].trim()),
        None => (rest, ""),
    };
    match expand_named(skills, name, args) {
        Ok(Some(expanded)) => expanded,
        _ => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xjpcode_coding_agent::core::source_info::SourceInfo;

    fn skill(name: &str, path: &str) -> Skill {
        Skill {
            name: name.to_string(),
            description: "说明".to_string(),
            file_path: path.to_string(),
            base_dir: "/tmp".to_string(),
            source_info: SourceInfo::synthetic(path, "user", None, Some("/tmp")),
            disable_model_invocation: false,
        }
    }

    #[test]
    fn skill_command_passes_unknown_names_through() {
        let skills = vec![skill("demo", "/definitely/missing/SKILL.md")];
        assert_eq!(
            expand_skill_command("/skill:nope hello", &skills),
            "/skill:nope hello"
        );
        assert_eq!(
            expand_skill_command("/skill:demo hi", &skills),
            "/skill:demo hi"
        );
    }

    #[test]
    fn list_shows_scope_description_and_path() {
        let skills = vec![skill("demo", "/tmp/demo/SKILL.md")];
        let text = list_text(&skills);
        assert!(text.contains("1. demo [全局]"));
        assert!(text.contains("路径: /tmp/demo/SKILL.md"));
    }
}
