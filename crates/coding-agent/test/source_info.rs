//! 移植自上游 `test/source-info.test.ts`（`core/source-info.ts`）。
//!
//! 上游那条回归测的是 Windows 上把文件路径的大小写折平；Windows 路径分支没搬
//! （`docs/port/milestone-3.md` 差异），所以这里测同一件事在 Unix 上能测的两半：
//! 合成标识原样保留、文件路径的比较键稳定。

use std::path::Path;

use tempfile::TempDir;
use xjpcode_coding_agent::core::source_info::get_resource_path_key;

#[test]
fn keeps_synthetic_identifiers_and_normalizes_file_paths() {
    assert_eq!(get_resource_path_key(Path::new("builtin:MCP")), "builtin:MCP");
    assert_ne!(
        get_resource_path_key(Path::new("builtin:MCP")),
        get_resource_path_key(Path::new("builtin:mcp"))
    );
    assert_eq!(
        get_resource_path_key(Path::new("<inline:Name>")),
        "<inline:Name>"
    );

    let dir = TempDir::new().unwrap();
    let file = dir.path().join("ext.ts");
    std::fs::write(&file, "x").unwrap();
    let detour = dir.path().join(".").join("ext.ts");
    assert_eq!(
        get_resource_path_key(&file),
        get_resource_path_key(&detour)
    );
}
