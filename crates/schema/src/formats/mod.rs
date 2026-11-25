//! 对应上游 `packages/schema/src/formats/` 目录。
//!
//! 上游没有目录入口文件，TypeScript 按路径直接解析目录；Rust 的子模块必须由某个文件里的 `mod` 声明
//! 引出，因此补这一个入口（与 `crates/ai/src/utils/mod.rs` 同理，见 `PORTING.md` 差异表 #5）。

pub mod index;
pub mod typebox;
