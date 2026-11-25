#![allow(dead_code)]
//! 测试里应用统一 patch（对应上游用例里的 `applyPatch`，来自 jsdiff）。
//!
//! 格式按统一 diff 的常见写法解析：`---`/`+++` 头、`@@ -a,b +c,d @@`、上下文/增/删行，以及
//! `\ No newline at end of file`。

/// 把 `patch` 应用到 `original`，返回新内容；对不上时报错。
pub fn apply_patch(original: &str, patch: &str) -> Result<String, String> {
    let original_lines: Vec<String> = original.split_inclusive('\n').map(str::to_string).collect();
    let mut output: Vec<String> = Vec::new();
    let mut cursor = 0usize;

    let patch_lines: Vec<&str> = patch.split('\n').collect();
    let mut index = 0usize;
    while index < patch_lines.len() {
        let line = patch_lines[index];
        if line.starts_with("--- ") || line.starts_with("+++ ") || line.is_empty() {
            index += 1;
            continue;
        }
        if !line.starts_with("@@ ") {
            return Err(format!("unexpected patch line: {line}"));
        }
        let (old_start, old_count) = parse_range(line, '-').ok_or("bad old range")?;
        let (new_start, _new_count) = parse_range(line, '+').ok_or("bad new range")?;
        index += 1;

        let apply_from = if old_count == 0 { old_start } else { old_start.saturating_sub(1) };
        // 跳过补丁 hunk 之前未提及的行。
        output.extend(original_lines[cursor..apply_from].iter().cloned());
        cursor = apply_from;

        let mut last_tag: Option<char> = None;
        while index < patch_lines.len() {
            let hunk_line = patch_lines[index];
            if hunk_line.starts_with("@@ ") {
                break;
            }
            if hunk_line.starts_with('\\') {
                // `\ No newline at end of file`：去掉上一条（加进来的或上下文的）输出末尾的换行；
                // 删掉的行不进输出，标记它不影响已经写下的内容。
                if last_tag != Some('-') {
                    if let Some(last) = output.last_mut() {
                        if last.ends_with('\n') {
                            last.pop();
                        }
                    }
                }
                index += 1;
                continue;
            }
            let (tag, text) = match hunk_line.chars().next() {
                Some(tag @ (' ' | '-' | '+')) => (tag, &hunk_line[1..]),
                _ => {
                    index += 1;
                    continue;
                }
            };
            match tag {
                ' ' => {
                    let found = original_lines.get(cursor).ok_or("context line past end")?;
                    if found.trim_end_matches('\n') != text {
                        return Err(format!(
                            "context mismatch at line {}: expected {text:?}, found {found:?}",
                            cursor + 1
                        ));
                    }
                    output.push(found.clone());
                    cursor += 1;
                    last_tag = Some(' ');
                }
                '-' => {
                    let found = original_lines.get(cursor).ok_or("removed line past end")?;
                    if found.trim_end_matches('\n') != text {
                        return Err(format!(
                            "removed line mismatch at line {}: expected {text:?}, found {found:?}",
                            cursor + 1
                        ));
                    }
                    cursor += 1;
                    last_tag = Some('-');
                }
                '+' => {
                    output.push(format!("{text}\n"));
                    last_tag = Some('+');
                }
                _ => unreachable!(),
            }
            index += 1;
        }
        let _ = new_start;
    }
    output.extend(original_lines[cursor..].iter().cloned());
    Ok(output.concat())
}

fn parse_range(header: &str, sign: char) -> Option<(usize, usize)> {
    let token = header
        .split(' ')
        .find(|token| token.starts_with(sign))?;
    let body = &token[1..];
    let mut parts = body.split(',');
    let start: usize = parts.next()?.parse().ok()?;
    let count: usize = parts.next().map(|value| value.parse().unwrap_or(1)).unwrap_or(1);
    Some((start, count))
}
