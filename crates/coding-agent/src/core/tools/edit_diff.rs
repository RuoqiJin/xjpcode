//! 对应上游 `src/core/tools/edit-diff.ts`：精确替换、模糊匹配、行尾保留、显示用 diff 与统一 patch。
//!
//! 上游的 jsdiff（`diffLines`、`structuredPatch`）换成 `similar`；显示 diff 的排版与 hunk 文案照上游
//! 逐行走。`applyPatch` 没有对应物，测试里用一个按统一 patch 格式应用的小工具代替（见
//! `test/utils/patch.rs`）。

use similar::{Algorithm, DiffOp, TextDiff};
use xjpcode_agent::ToolError;

use crate::core::tools::path_utils::resolve_to_cwd;
use crate::utils::text::split_bom;

/// pi: `normalizeToLF`
pub fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// pi: `TextReplacement`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextReplacement {
    pub match_index: usize,
    pub match_length: usize,
    pub new_text: String,
}

/// pi: `Edit`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub old_text: String,
    pub new_text: String,
}

/// pi: `AppliedEditsResult`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedEditsResult {
    pub base_content: String,
    pub new_content: String,
    pub replacements: Vec<TextReplacement>,
}

/// pi: `applyReplacementsToOriginal`
pub fn apply_replacements_to_original(
    original_content: &str,
    replacements: &[TextReplacement],
) -> String {
    let mut crlf_before: usize = 0;
    let mut next_crlf: Option<usize> = original_content.find("\r\n");
    let to_original =
        |offset: usize, crlf_before: &mut usize, next_crlf: &mut Option<usize>| -> usize {
            while let Some(position) = *next_crlf {
                if position - *crlf_before < offset {
                    *crlf_before += 1;
                    *next_crlf = original_content[position + 2..]
                        .find("\r\n")
                        .map(|found| found + position + 2);
                } else {
                    break;
                }
            }
            offset + *crlf_before
        };

    let mut parts: Vec<String> = Vec::new();
    let mut ends_with_cr = false;
    let mut push = |part: &str, ends_with_cr: &mut bool| {
        if part.is_empty() {
            return;
        }
        if *ends_with_cr && part.starts_with('\n') {
            parts.push("\n".to_string());
        }
        parts.push(part.to_string());
        *ends_with_cr = part.ends_with('\r');
    };
    let mut position = 0usize;
    for replacement in replacements {
        let start = to_original(replacement.match_index, &mut crlf_before, &mut next_crlf);
        push(&original_content[position..start], &mut ends_with_cr);
        let ending = get_line_ending_at(original_content, start);
        let new_text = if ending == "\n" {
            replacement.new_text.clone()
        } else {
            replacement.new_text.replace('\n', ending)
        };
        push(&new_text, &mut ends_with_cr);
        position = to_original(
            replacement.match_index + replacement.match_length,
            &mut crlf_before,
            &mut next_crlf,
        );
    }
    push(&original_content[position..], &mut ends_with_cr);
    parts.concat()
}

/// pi: `getLineEndingAt`
fn get_line_ending_at(content: &str, offset: usize) -> &'static str {
    let bytes = content.as_bytes();
    let mut line_feed = content[offset.min(content.len())..]
        .find('\n')
        .map(|found| found + offset.min(content.len()));
    if line_feed.is_none() {
        line_feed = content[..offset.min(content.len())].rfind('\n');
    }
    match line_feed {
        None => {
            if content.contains('\r') {
                "\r"
            } else {
                "\n"
            }
        }
        Some(index) => {
            if index > 0 && bytes[index - 1] == b'\r' {
                "\r\n"
            } else {
                "\n"
            }
        }
    }
}

/// pi: `normalizeForFuzzyMatch`
pub fn normalize_for_fuzzy_match(text: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    let normalized: String = text.nfkc().collect();
    normalized
        .split('\n')
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
        .chars()
        .map(|character| match character {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' => '-',
            '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

/// pi: `splitLinesWithEndings`
fn split_lines_with_endings(content: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    for (index, character) in content.char_indices() {
        if character == '\n' {
            lines.push(&content[start..index + 1]);
            start = index + 1;
        }
    }
    if start < content.len() {
        lines.push(&content[start..]);
    }
    lines
}

#[derive(Debug, Clone, Copy)]
struct LineSpan {
    start: usize,
    end: usize,
}

fn get_line_spans(content: &str) -> Vec<LineSpan> {
    let mut offset = 0usize;
    split_lines_with_endings(content)
        .into_iter()
        .map(|line| {
            let span = LineSpan {
                start: offset,
                end: offset + line.len(),
            };
            offset = span.end;
            span
        })
        .collect()
}

fn get_replacement_line_range(
    lines: &[LineSpan],
    replacement: &TextReplacement,
) -> Result<(usize, usize), ToolError> {
    let replacement_start = replacement.match_index;
    let replacement_end = replacement.match_index + replacement.match_length;

    let mut start_line = None;
    for (index, line) in lines.iter().enumerate() {
        if replacement_start >= line.start && replacement_start < line.end {
            start_line = Some(index);
            break;
        }
    }
    let Some(start_line) = start_line else {
        return Err(ToolError::from(
            "Replacement range is outside the base content.",
        ));
    };

    let mut end_line = start_line;
    while end_line < lines.len() && lines[end_line].end < replacement_end {
        end_line += 1;
    }
    if end_line >= lines.len() {
        return Err(ToolError::from(
            "Replacement range is outside the base content.",
        ));
    }

    Ok((start_line, end_line + 1))
}

fn apply_replacements(content: &str, replacements: &[TextReplacement], offset: usize) -> String {
    let mut result = content.to_string();
    for replacement in replacements.iter().rev() {
        let match_index = replacement.match_index - offset;
        result.replace_range(
            match_index..match_index + replacement.match_length,
            &replacement.new_text,
        );
    }
    result
}

/// pi: `widenReplacementsToLines`
fn widen_replacements_to_lines(
    original_content: &str,
    base_content: &str,
    replacements: &[TextReplacement],
) -> Result<Vec<TextReplacement>, ToolError> {
    let original_lines = get_line_spans(original_content);
    let base_lines = get_line_spans(base_content);
    if original_lines.len() != base_lines.len() {
        return Err(ToolError::from(
            "Cannot preserve unchanged lines because the base content has a different line count.",
        ));
    }

    struct Group {
        start_line: usize,
        end_line: usize,
        replacements: Vec<TextReplacement>,
    }
    let mut groups: Vec<Group> = Vec::new();
    let mut sorted = replacements.to_vec();
    sorted.sort_by_key(|replacement| replacement.match_index);
    for replacement in sorted {
        let (start_line, end_line) = get_replacement_line_range(&base_lines, &replacement)?;
        if let Some(current) = groups.last_mut() {
            if start_line < current.end_line {
                current.end_line = current.end_line.max(end_line);
                current.replacements.push(replacement);
                continue;
            }
        }
        groups.push(Group {
            start_line,
            end_line,
            replacements: vec![replacement],
        });
    }

    let mut widened = Vec::new();
    for group in groups {
        let group_start_offset = base_lines[group.start_line].start;
        let group_end_offset = base_lines[group.end_line - 1].end;
        let original_start = original_lines[group.start_line].start;
        widened.push(TextReplacement {
            match_index: original_start,
            match_length: original_lines[group.end_line - 1].end - original_start,
            new_text: apply_replacements(
                &base_content[group_start_offset..group_end_offset],
                &group.replacements,
                group_start_offset,
            ),
        });
    }
    Ok(widened)
}

/// pi: `FuzzyMatchResult`
#[derive(Debug, Clone)]
pub struct FuzzyMatchResult {
    pub found: bool,
    pub index: usize,
    pub match_length: usize,
    pub used_fuzzy_match: bool,
    pub content_for_replacement: String,
}

/// pi: `fuzzyFindText`
pub fn fuzzy_find_text(content: &str, old_text: &str) -> FuzzyMatchResult {
    if let Some(index) = content.find(old_text) {
        return FuzzyMatchResult {
            found: true,
            index,
            match_length: old_text.len(),
            used_fuzzy_match: false,
            content_for_replacement: content.to_string(),
        };
    }

    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old_text = normalize_for_fuzzy_match(old_text);
    match fuzzy_content.find(&fuzzy_old_text) {
        Some(index) => FuzzyMatchResult {
            found: true,
            index,
            match_length: fuzzy_old_text.len(),
            used_fuzzy_match: true,
            content_for_replacement: fuzzy_content,
        },
        None => FuzzyMatchResult {
            found: false,
            index: 0,
            match_length: 0,
            used_fuzzy_match: false,
            content_for_replacement: content.to_string(),
        },
    }
}

fn count_occurrences(content: &str, old_text: &str) -> usize {
    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old_text = normalize_for_fuzzy_match(old_text);
    if fuzzy_old_text.is_empty() {
        return fuzzy_content.chars().count() + 1;
    }
    fuzzy_content.matches(&fuzzy_old_text).count()
}

fn not_found_error(path: &str, edit_index: usize, total_edits: usize) -> ToolError {
    if total_edits == 1 {
        ToolError::from(format!(
            "Could not find the exact text in {path}. The old text must match exactly including all whitespace and newlines."
        ))
    } else {
        ToolError::from(format!(
            "Could not find edits[{edit_index}] in {path}. The oldText must match exactly including all whitespace and newlines."
        ))
    }
}

fn duplicate_error(
    path: &str,
    edit_index: usize,
    total_edits: usize,
    occurrences: usize,
) -> ToolError {
    if total_edits == 1 {
        ToolError::from(format!(
            "Found {occurrences} occurrences of the text in {path}. The text must be unique. Please provide more context to make it unique."
        ))
    } else {
        ToolError::from(format!(
            "Found {occurrences} occurrences of edits[{edit_index}] in {path}. Each oldText must be unique. Please provide more context to make it unique."
        ))
    }
}

fn empty_old_text_error(path: &str, edit_index: usize, total_edits: usize) -> ToolError {
    if total_edits == 1 {
        ToolError::from(format!("oldText must not be empty in {path}."))
    } else {
        ToolError::from(format!(
            "edits[{edit_index}].oldText must not be empty in {path}."
        ))
    }
}

fn no_change_error(path: &str, total_edits: usize) -> ToolError {
    if total_edits == 1 {
        ToolError::from(format!(
            "No changes made to {path}. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected."
        ))
    } else {
        ToolError::from(format!(
            "No changes made to {path}. The replacements produced identical content."
        ))
    }
}

/// pi: `applyEditsToNormalizedContent`
pub fn apply_edits_to_normalized_content(
    normalized_content: &str,
    edits: &[Edit],
    path: &str,
) -> Result<AppliedEditsResult, ToolError> {
    let normalized_edits: Vec<Edit> = edits
        .iter()
        .map(|edit| Edit {
            old_text: normalize_to_lf(&edit.old_text),
            new_text: normalize_to_lf(&edit.new_text),
        })
        .collect();

    for (index, edit) in normalized_edits.iter().enumerate() {
        if edit.old_text.is_empty() {
            return Err(empty_old_text_error(path, index, normalized_edits.len()));
        }
    }

    let initial_matches: Vec<FuzzyMatchResult> = normalized_edits
        .iter()
        .map(|edit| fuzzy_find_text(normalized_content, &edit.old_text))
        .collect();
    let used_fuzzy_match = initial_matches.iter().any(|result| result.used_fuzzy_match);
    let replacement_base_content = if used_fuzzy_match {
        normalize_for_fuzzy_match(normalized_content)
    } else {
        normalized_content.to_string()
    };

    struct MatchedEdit {
        edit_index: usize,
        match_index: usize,
        match_length: usize,
        new_text: String,
    }
    let mut matched_edits: Vec<MatchedEdit> = Vec::new();
    for (index, edit) in normalized_edits.iter().enumerate() {
        let match_result = fuzzy_find_text(&replacement_base_content, &edit.old_text);
        if !match_result.found {
            return Err(not_found_error(path, index, normalized_edits.len()));
        }
        let occurrences = count_occurrences(&replacement_base_content, &edit.old_text);
        if occurrences > 1 {
            return Err(duplicate_error(
                path,
                index,
                normalized_edits.len(),
                occurrences,
            ));
        }
        matched_edits.push(MatchedEdit {
            edit_index: index,
            match_index: match_result.index,
            match_length: match_result.match_length,
            new_text: edit.new_text.clone(),
        });
    }

    matched_edits.sort_by_key(|edit| edit.match_index);
    for pair in matched_edits.windows(2) {
        let previous = &pair[0];
        let current = &pair[1];
        if previous.match_index + previous.match_length > current.match_index {
            return Err(ToolError::from(format!(
                "edits[{}] and edits[{}] overlap in {path}. Merge them into one edit or target disjoint regions.",
                previous.edit_index, current.edit_index
            )));
        }
    }

    let matched: Vec<TextReplacement> = matched_edits
        .iter()
        .map(|edit| TextReplacement {
            match_index: edit.match_index,
            match_length: edit.match_length,
            new_text: edit.new_text.clone(),
        })
        .collect();
    let base_content = normalized_content.to_string();
    let replacements = if used_fuzzy_match {
        widen_replacements_to_lines(normalized_content, &replacement_base_content, &matched)?
    } else {
        matched
    };
    let new_content = apply_replacements(&base_content, &replacements, 0);

    if base_content == new_content {
        return Err(no_change_error(path, normalized_edits.len()));
    }

    Ok(AppliedEditsResult {
        base_content,
        new_content,
        replacements,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PartKind {
    Unchanged,
    Removed,
    Added,
}

#[derive(Debug, Clone)]
struct Part {
    kind: PartKind,
    value: String,
}

fn diff_parts(old_content: &str, new_content: &str) -> Vec<Part> {
    let old_lines = split_lines_with_endings(old_content);
    let new_lines = split_lines_with_endings(new_content);
    let mut config = TextDiff::configure();
    config
        .algorithm(Algorithm::Patience)
        .timeout(std::time::Duration::from_millis(DIFF_TIMEOUT_MS));
    let diff = config.diff_lines(old_content, new_content);
    let mut parts = Vec::new();
    let mut push = |kind: PartKind, lines: &[&str], range: std::ops::Range<usize>| {
        if range.is_empty() {
            return;
        }
        let value = lines[range].concat();
        if value.is_empty() {
            return;
        }
        parts.push(Part { kind, value });
    };
    for operation in diff.ops() {
        match operation {
            DiffOp::Equal {
                old_index,
                new_index: _,
                len,
            } => push(
                PartKind::Unchanged,
                &old_lines,
                *old_index..*old_index + *len,
            ),
            DiffOp::Delete {
                old_index,
                old_len,
                new_index: _,
            } => push(
                PartKind::Removed,
                &old_lines,
                *old_index..*old_index + *old_len,
            ),
            DiffOp::Insert {
                old_index: _,
                new_index,
                new_len,
            } => push(
                PartKind::Added,
                &new_lines,
                *new_index..*new_index + *new_len,
            ),
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                push(
                    PartKind::Removed,
                    &old_lines,
                    *old_index..*old_index + *old_len,
                );
                push(
                    PartKind::Added,
                    &new_lines,
                    *new_index..*new_index + *new_len,
                );
            }
        }
    }
    parts
}

fn count_lines(text: &str) -> usize {
    text.bytes().filter(|byte| *byte == b'\n').count() + 1
}

fn count_line_feeds(text: &str, start: usize, end: usize) -> usize {
    text[start.min(text.len())..end.min(text.len())]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
}

/// pi: `formatDisplayDiff`
fn format_display_diff(
    parts: &[Part],
    old_start_line: usize,
    new_start_line: usize,
    line_num_width: usize,
    context_lines: usize,
) -> (Vec<String>, Option<usize>) {
    let mut output: Vec<String> = Vec::new();
    let mut old_line_num = old_start_line;
    let mut new_line_num = new_start_line;
    let mut last_was_change = false;
    let mut first_changed_line: Option<usize> = None;

    for (index, part) in parts.iter().enumerate() {
        let mut raw: Vec<&str> = part.value.split('\n').collect();
        if raw.last() == Some(&"") {
            raw.pop();
        }

        if part.kind != PartKind::Unchanged {
            if first_changed_line.is_none() {
                first_changed_line = Some(new_line_num);
            }
            for line in &raw {
                if part.kind == PartKind::Added {
                    output.push(format!(
                        "+{:>width$} {}",
                        new_line_num,
                        line,
                        width = line_num_width
                    ));
                    new_line_num += 1;
                } else {
                    output.push(format!(
                        "-{:>width$} {}",
                        old_line_num,
                        line,
                        width = line_num_width
                    ));
                    old_line_num += 1;
                }
            }
            last_was_change = true;
        } else {
            let next_part_is_change = parts
                .get(index + 1)
                .is_some_and(|next| next.kind != PartKind::Unchanged);
            let has_leading_change = last_was_change;
            let has_trailing_change = next_part_is_change;

            if has_leading_change && has_trailing_change {
                if raw.len() <= context_lines * 2 {
                    for line in &raw {
                        output.push(format!(
                            " {:>width$} {}",
                            old_line_num,
                            line,
                            width = line_num_width
                        ));
                        old_line_num += 1;
                        new_line_num += 1;
                    }
                } else {
                    let leading_lines = &raw[..context_lines];
                    let trailing_lines = &raw[raw.len() - context_lines..];
                    let skipped_lines = raw.len() - leading_lines.len() - trailing_lines.len();

                    for line in leading_lines {
                        output.push(format!(
                            " {:>width$} {}",
                            old_line_num,
                            line,
                            width = line_num_width
                        ));
                        old_line_num += 1;
                        new_line_num += 1;
                    }
                    output.push(format!(" {} ...", " ".repeat(line_num_width)));
                    old_line_num += skipped_lines;
                    new_line_num += skipped_lines;
                    for line in trailing_lines {
                        output.push(format!(
                            " {:>width$} {}",
                            old_line_num,
                            line,
                            width = line_num_width
                        ));
                        old_line_num += 1;
                        new_line_num += 1;
                    }
                }
            } else if has_leading_change {
                let shown_lines = &raw[..raw.len().min(context_lines)];
                let skipped_lines = raw.len() - shown_lines.len();
                for line in shown_lines {
                    output.push(format!(
                        " {:>width$} {}",
                        old_line_num,
                        line,
                        width = line_num_width
                    ));
                    old_line_num += 1;
                    new_line_num += 1;
                }
                if skipped_lines > 0 {
                    output.push(format!(" {} ...", " ".repeat(line_num_width)));
                    old_line_num += skipped_lines;
                    new_line_num += skipped_lines;
                }
            } else if has_trailing_change {
                let skipped_lines = raw.len().saturating_sub(context_lines);
                if skipped_lines > 0 {
                    output.push(format!(" {} ...", " ".repeat(line_num_width)));
                    old_line_num += skipped_lines;
                    new_line_num += skipped_lines;
                }
                for line in &raw[skipped_lines..] {
                    output.push(format!(
                        " {:>width$} {}",
                        old_line_num,
                        line,
                        width = line_num_width
                    ));
                    old_line_num += 1;
                    new_line_num += 1;
                }
            } else {
                old_line_num += raw.len();
                new_line_num += raw.len();
            }

            last_was_change = false;
        }
    }

    (output, first_changed_line)
}

/// pi: `generateUnifiedPatch`
pub fn generate_unified_patch(
    path: &str,
    old_content: &str,
    new_content: &str,
    context_lines: usize,
) -> String {
    let mut config = TextDiff::configure();
    config
        .algorithm(Algorithm::Patience)
        .timeout(std::time::Duration::from_millis(DIFF_TIMEOUT_MS));
    config
        .diff_lines(old_content, new_content)
        .unified_diff()
        .context_radius(context_lines)
        .header(path, path)
        .to_string()
}

/// pi: `generateDiffString`
pub fn generate_diff_string(
    old_content: &str,
    new_content: &str,
    context_lines: usize,
) -> (String, Option<usize>) {
    let line_num_width = count_lines(old_content)
        .max(count_lines(new_content))
        .to_string()
        .len();
    let parts = diff_parts(old_content, new_content);
    let (output, first_changed_line) =
        format_display_diff(&parts, 1, 1, line_num_width, context_lines);
    (output.join("\n"), first_changed_line)
}

/// pi: `EditDiff`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditDiff {
    pub diff: String,
    pub patch: String,
    pub first_changed_line: Option<usize>,
}

/// 上游 `DIFF_WINDOW_MARGIN_LINES`
const DIFF_WINDOW_MARGIN_LINES: usize = 32;

/// diff 的时限：超过就退化成整块替换。上游 jsdiff 在重复行很多时也会很慢；显示用 diff 不需要
/// 最优对齐（见 `docs/port/milestone-3.md`）。
const DIFF_TIMEOUT_MS: u64 = 500;

/// pi: `generateEditDiff`
pub fn generate_edit_diff(
    path: &str,
    base_content: &str,
    new_content: &str,
    replacements: &[TextReplacement],
    context_lines: usize,
) -> EditDiff {
    let line_num_width = count_lines(base_content)
        .max(count_lines(new_content))
        .to_string()
        .len();
    let skip_marker = format!(" {} ...", " ".repeat(line_num_width));
    let mut output: Vec<String> = Vec::new();
    let mut patch = String::new();
    let mut first_changed_line: Option<usize> = None;
    let mut wrote_header = false;

    for window in get_diff_windows(base_content, replacements) {
        let old_text = &base_content[window.old_start..window.old_end];
        let new_text = &new_content[window.new_start..window.new_end];
        let parts = diff_parts(old_text, new_text);
        let (shown, changed) = format_display_diff(
            &parts,
            window.old_line + 1,
            window.new_line + 1,
            line_num_width,
            context_lines,
        );
        first_changed_line = first_changed_line.or(changed);
        let mut shown = shown;
        if output.last() == Some(&skip_marker) && shown.first() == Some(&skip_marker) {
            shown.remove(0);
        }
        output.extend(shown);

        let hunk_text = generate_unified_patch(path, old_text, new_text, context_lines);
        for line in hunk_text.lines() {
            if line.starts_with("@@ ") {
                patch.push_str(&shift_hunk_header(line, window.old_line, window.new_line));
                patch.push('\n');
            } else if line.starts_with("--- ") || line.starts_with("+++ ") {
                if !wrote_header {
                    patch.push_str(line);
                    patch.push('\n');
                    if line.starts_with("+++ ") {
                        wrote_header = true;
                    }
                }
            } else {
                patch.push_str(line);
                patch.push('\n');
            }
        }
    }

    EditDiff {
        diff: output.join("\n"),
        patch,
        first_changed_line,
    }
}

fn shift_hunk_header(header: &str, old_offset: usize, new_offset: usize) -> String {
    // `@@ -a,b +c,d @@`
    let mut parts = header.split(' ');
    let _ = parts.next();
    let old_spec = parts.next().unwrap_or("-0,0").trim_start_matches('-');
    let new_spec = parts.next().unwrap_or("+0,0").trim_start_matches('+');
    let shift = |spec: &str, offset: usize| -> String {
        let mut values = spec.split(',');
        let start: usize = values
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let count = values.next().map(ToString::to_string);
        let start = start + offset;
        match count {
            Some(count) => format!("{start},{count}"),
            None => format!("{start}"),
        }
    };
    format!(
        "@@ -{} +{} @@",
        shift(old_spec, old_offset),
        shift(new_spec, new_offset)
    )
}

/// pi: `DiffWindow`
struct DiffWindow {
    old_start: usize,
    old_end: usize,
    old_line: usize,
    new_start: usize,
    new_end: usize,
    new_line: usize,
}

/// 往前退到最近的字符边界（上游按 UTF-16 下标搜换行，落在代理对中间也不会崩；
/// Rust 按字节下标切片，必须先把下标退回边界）。
fn floor_char_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn line_start_before(text: &str, offset: usize) -> usize {
    if offset == 0 {
        return 0;
    }
    text[..offset.min(text.len())]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0)
}

/// pi: `getDiffWindows`
fn get_diff_windows(base_content: &str, replacements: &[TextReplacement]) -> Vec<DiffWindow> {
    let mut windows: Vec<DiffWindow> = Vec::new();
    let mut offset_delta: isize = 0;
    let mut line_delta: isize = 0;
    let mut cursor_offset = 0usize;
    let mut cursor_line = 0usize;
    for replacement in replacements {
        let match_end = replacement.match_index + replacement.match_length;
        let mut start = line_start_before(base_content, replacement.match_index);
        for _ in 0..DIFF_WINDOW_MARGIN_LINES {
            if start == 0 {
                break;
            }
            start = line_start_before(base_content, start - 1);
        }
        let mut end = match_end;
        for index in 0..=DIFF_WINDOW_MARGIN_LINES {
            if end >= base_content.len() {
                break;
            }
            let search_from = if index == 0 {
                floor_char_boundary(
                    base_content,
                    match_end.saturating_sub(1).max(replacement.match_index),
                )
            } else {
                end
            };
            end = base_content[search_from..]
                .find('\n')
                .map(|found| found + search_from + 1)
                .unwrap_or(base_content.len());
        }
        let replacement_offset_delta =
            replacement.new_text.len() as isize - replacement.match_length as isize;
        let replacement_line_delta =
            count_line_feeds(&replacement.new_text, 0, replacement.new_text.len()) as isize
                - count_line_feeds(
                    base_content,
                    replacement.match_index,
                    replacement.match_index + replacement.match_length,
                ) as isize;
        let mut merged = false;
        if let Some(previous) = windows.last_mut() {
            if start <= previous.old_end {
                previous.old_end = previous.old_end.max(end);
                previous.new_end =
                    (previous.old_end as isize + offset_delta + replacement_offset_delta) as usize;
                merged = true;
            }
        }
        if !merged {
            cursor_line += count_line_feeds(base_content, cursor_offset, start);
            cursor_offset = start;
            windows.push(DiffWindow {
                old_start: start,
                old_end: end,
                old_line: cursor_line,
                new_start: (start as isize + offset_delta) as usize,
                new_end: (end as isize + offset_delta + replacement_offset_delta) as usize,
                new_line: (cursor_line as isize + line_delta) as usize,
            });
        }
        offset_delta += replacement_offset_delta;
        line_delta += replacement_line_delta;
    }
    windows
}

/// pi: `EditDiffResult`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditDiffResult {
    pub diff: String,
    pub first_changed_line: Option<usize>,
}

/// pi: `EditDiffError`——本移植里用 `Err(String)` 表示。
pub type EditDiffError = String;

/// pi: `computeEditsDiff`
pub async fn compute_edits_diff(
    path: &str,
    edits: &[Edit],
    cwd: &str,
) -> Result<EditDiffResult, EditDiffError> {
    let absolute_path = resolve_to_cwd(path, cwd);

    let readable = tokio::fs::File::open(&absolute_path).await;
    if let Err(error) = readable {
        return Err(format!(
            "Could not edit file: {path}. {}.",
            describe_access_error(&error)
        ));
    }
    let raw_content = match tokio::fs::read(&absolute_path).await {
        Ok(content) => String::from_utf8_lossy(&content).into_owned(),
        Err(error) => {
            return Err(format!(
                "Could not edit file: {path}. {}.",
                describe_access_error(&error)
            ))
        }
    };

    let (_, content) = split_bom(&raw_content);
    let normalized_content = normalize_to_lf(content);
    let applied = match apply_edits_to_normalized_content(&normalized_content, edits, path) {
        Ok(applied) => applied,
        Err(error) => return Err(error.message().to_string()),
    };
    let edit_diff = generate_edit_diff(
        path,
        &applied.base_content,
        &applied.new_content,
        &applied.replacements,
        4,
    );
    Ok(EditDiffResult {
        diff: edit_diff.diff,
        first_changed_line: edit_diff.first_changed_line,
    })
}

/// pi: `computeEditDiff`
pub async fn compute_edit_diff(
    path: &str,
    old_text: &str,
    new_text: &str,
    cwd: &str,
) -> Result<EditDiffResult, EditDiffError> {
    compute_edits_diff(
        path,
        &[Edit {
            old_text: old_text.to_string(),
            new_text: new_text.to_string(),
        }],
        cwd,
    )
    .await
}

/// 上游错误对象上的 `code`：`Error code: ENOENT`，没有 code 时是 `Error: 原话`。
pub fn describe_access_error(error: &std::io::Error) -> String {
    match error_code(error) {
        Some(code) => format!("Error code: {code}"),
        None => format!("Error: {error}"),
    }
}

/// Unix 上的 `error.code`。
pub fn error_code(error: &std::io::Error) -> Option<&'static str> {
    match error.raw_os_error() {
        Some(2) => Some("ENOENT"),
        Some(20) => Some("ENOTDIR"),
        Some(13) => Some("EACCES"),
        Some(1) => Some("EPERM"),
        Some(21) => Some("EISDIR"),
        _ => None,
    }
}

/// 文件系统错误的展示：带 code 时写成 `ENOENT: No such file or directory (os error 2)`。
pub fn describe_io_error(error: &std::io::Error) -> String {
    match error_code(error) {
        Some(code) => format!("{code}: {error}"),
        None => error.to_string(),
    }
}
