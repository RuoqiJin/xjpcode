//! 对应上游 `test/edit-diff-window.test.ts`：只对改动附近做 diff，结果与整份 diff 一致。
//!
//! 上游用 jsdiff 的 `applyPatch`；这里用 `test/utils/patch.rs` 里按统一 diff 格式应用的小工具。

#[path = "utils/mod.rs"]
mod utils;

use xjpcode_coding_agent::core::tools::edit_diff::{
    generate_diff_string, generate_edit_diff, generate_unified_patch, TextReplacement,
};

fn create_random(seed: u32) -> impl FnMut(usize) -> usize {
    let mut state = seed;
    move |max| {
        state = state.wrapping_add(0x6d2b_79f5);
        let t = (state ^ (state >> 15)).wrapping_mul(1 | state);
        let t = t.wrapping_add((t ^ (t >> 7)).wrapping_mul(61 | t)) ^ t;
        ((((t ^ (t >> 14)) as f64) / 4_294_967_296.0) * max as f64) as usize
    }
}

fn apply_replacements(content: &str, replacements: &[TextReplacement]) -> String {
    let mut result = String::new();
    let mut position = 0usize;
    for replacement in replacements {
        result.push_str(&content[position..replacement.match_index]);
        result.push_str(&replacement.new_text);
        position = replacement.match_index + replacement.match_length;
    }
    result.push_str(&content[position..]);
    result
}

struct RandomCase {
    content: String,
    replacements: Vec<TextReplacement>,
    new_content: String,
}

/// 随机内容与排好序、互不重叠、非空的替换；`distinct` 时每行都不同。
fn random_case(random: &mut impl FnMut(usize) -> usize, distinct: bool) -> RandomCase {
    let line_count = random(400);
    let lines: Vec<String> = (0..line_count)
        .map(|index| {
            if distinct {
                format!("line {index}")
            } else {
                ["a", "b", "", "}"][random(4)].to_string()
            }
        })
        .collect();
    let mut content = lines.join("\n");
    if random(2) == 0 {
        content.push('\n');
    }
    let mut replacements: Vec<TextReplacement> = Vec::new();
    let mut position = 0usize;
    let count = 1 + random(4);
    for index in 0..count {
        if position >= content.len() {
            break;
        }
        let match_index = position + random((content.len() - position).min(300));
        if match_index >= content.len() {
            break;
        }
        let match_length = 1 + random((content.len() - match_index).min(120));
        let new_lines: Vec<String> = (0..random(4)).map(|j| format!("new {index} {j}")).collect();
        let new_text = if random(3) == 0 {
            String::new()
        } else {
            let mut text = new_lines.join("\n");
            if random(2) == 0 {
                text.push('\n');
            }
            text
        };
        replacements.push(TextReplacement {
            match_index,
            match_length,
            new_text,
        });
        position = match_index + match_length;
    }
    RandomCase {
        new_content: apply_replacements(&content, &replacements),
        content,
        replacements,
    }
}

#[test]
fn matches_the_whole_file_diff_and_patch_when_lines_are_distinct() {
    let mut random = create_random(1);
    for run in 0..2000 {
        let case = random_case(&mut random, true);
        if case.content == case.new_content {
            continue;
        }
        let windowed = generate_edit_diff(
            "f.txt",
            &case.content,
            &case.new_content,
            &case.replacements,
            4,
        );
        let (whole_diff, whole_first_changed_line) = generate_diff_string(&case.content, &case.new_content, 4);
        assert_eq!(windowed.diff, whole_diff, "run {run}");
        assert_eq!(
            windowed.patch,
            generate_unified_patch("f.txt", &case.content, &case.new_content, 4),
            "run {run}"
        );
        assert_eq!(windowed.first_changed_line, whole_first_changed_line, "run {run}");
    }
}

#[test]
fn produces_a_patch_that_applies_when_lines_repeat() {
    let mut random = create_random(2);
    for run in 0..2000 {
        let case = random_case(&mut random, false);
        if case.content == case.new_content {
            continue;
        }
        let (_, edit_diff) = (
            &case.content,
            generate_edit_diff("f.txt", &case.content, &case.new_content, &case.replacements, 4),
        );
        let applied = utils::patch::apply_patch(&case.content, &edit_diff.patch)
            .unwrap_or_else(|error| panic!("run {run}: {error}\n{}", edit_diff.patch));
        assert_eq!(
            applied, case.new_content,
            "run {run}\n--- patch ---\n{}\n--- original ---\n{:?}\n--- new ---\n{:?}",
            edit_diff.patch, case.content, case.new_content
        );
    }
}

#[test]
fn handles_a_replacement_of_many_lines() {
    let new_text = "x\n".repeat(150_000);
    let replacements = vec![TextReplacement {
        match_index: 0,
        match_length: 2,
        new_text: new_text.clone(),
    }];
    let edit_diff = generate_edit_diff("f.txt", "a\nb\n", &format!("{new_text}b\n"), &replacements, 4);
    assert_eq!(
        utils::patch::apply_patch("a\nb\n", &edit_diff.patch).unwrap(),
        format!("{new_text}b\n")
    );
}

#[test]
fn diffs_a_small_change_in_a_20mb_file() {
    let filler = format!("{}\n", "x".repeat(99));
    let half = filler.repeat(100_000);
    let base = format!("{half}TARGET = 1\n{half}");
    let replacement = TextReplacement {
        match_index: half.len(),
        match_length: 10,
        new_text: "TARGET = 2".to_string(),
    };
    let changed = format!("{half}TARGET = 2\n{half}");
    let edit_diff = generate_edit_diff("big.txt", &base, &changed, &[replacement], 4);
    let changed_lines: Vec<&str> = edit_diff
        .diff
        .split('\n')
        .filter(|line| line.starts_with('+') || line.starts_with('-'))
        .collect();
    assert_eq!(changed_lines, vec!["-100001 TARGET = 1", "+100001 TARGET = 2"]);
}
