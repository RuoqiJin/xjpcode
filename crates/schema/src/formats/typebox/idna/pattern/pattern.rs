//! 对应上游 `packages/schema/src/formats/typebox/idna/pattern/pattern.ts`。
//!
//! 改编自 TypeBox 1.3.27（MIT），许可证见 `THIRD_PARTY_NOTICES/typebox-LICENSE`。
//!
//! 上游把若干子模式拼成一个新正则（`RE_EUROPEAN_NUMBER`、`RE_PERMITTED_CATEGORY`）：Rust 的
//! `regress` 不暴露 `.source`，所以这里直接写下拼好的源码，并在注释里标出它的来源。

use std::sync::LazyLock;

use regress::Regex;

macro_rules! pattern {
    ($source:expr $(,)?) => {
        LazyLock::new(|| Regex::with_flags($source, "u").expect("idna pattern"))
    };
}

// ---------------------------------------------------------------- 校验规则

pub static RE_RULE_HYPHEN_PLACEMENT: LazyLock<Regex> = pattern!(r"^(?!-).*(?<!-)$");
pub static RE_RULE_NOT_RESERVED_ACE: LazyLock<Regex> = pattern!(r"^(?!..--)");
pub static RE_ASCII_LDH: LazyLock<Regex> = pattern!(r"^[a-zA-Z0-9-]*$");

// ---------------------------------------------------------------- ASCII 与编码

pub static RE_ASCII: LazyLock<Regex> = pattern!(r"^\p{ASCII}*$");
pub static RE_NON_ASCII: LazyLock<Regex> = pattern!(r"[^\p{ASCII}]");

// ---------------------------------------------------------------- 结构与 Unicode 控制符

pub static RE_HIGH_SURROGATE: LazyLock<Regex> = pattern!(r"[\u{d800}-\u{dbff}]");
pub static RE_REGIONAL_INDICATOR: LazyLock<Regex> = pattern!(r"[\u{1f1e6}-\u{1f1ff}]");
pub static RE_VARIATION_SELECTOR: LazyLock<Regex> = pattern!(r"[\u{fe00}-\u{fe0f}]");
pub static RE_ZERO_WIDTH_JOINER: LazyLock<Regex> = pattern!(r"\u{200d}");

// ---------------------------------------------------------------- 数字与数码

pub static RE_ASCII_DIGIT: LazyLock<Regex> = pattern!(r"[0-9]");
pub static RE_ARABIC_INDIC_DIGIT: LazyLock<Regex> = pattern!(r"[\u{0660}-\u{0669}]");
pub static RE_EXT_ARABIC_INDIC_DIGIT: LazyLock<Regex> = pattern!(r"[\u{06f0}-\u{06f9}]");

// ---------------------------------------------------------------- 分隔符

pub static RE_COMMON_SEPARATOR: LazyLock<Regex> = pattern!(r"[\u{002e}\u{002c}\u{003a}\u{002f}]");
pub static RE_EUROPEAN_SEPARATOR: LazyLock<Regex> = pattern!(r"[\u{002d}\u{002b}]");

// ---------------------------------------------------------------- 通用类别

pub static RE_MARK_NONSPACING: LazyLock<Regex> = pattern!(r"\p{Mn}");
pub static RE_MARK_SPACING_COMBINING: LazyLock<Regex> = pattern!(r"\p{Mc}");
pub static RE_MARK_ENCLOSING: LazyLock<Regex> = pattern!(r"\p{Me}");
pub static RE_COMBINING_MARK: LazyLock<Regex> = pattern!(r"[\p{Mn}\p{Mc}\p{Me}]");
pub static RE_LETTER: LazyLock<Regex> = pattern!(r"\p{L}");
pub static RE_NUMBER_DECIMAL: LazyLock<Regex> = pattern!(r"\p{Nd}");

// ---------------------------------------------------------------- 文字

pub static RE_SCRIPT_GREEK: LazyLock<Regex> = pattern!(r"\p{Script=Greek}");
pub static RE_SCRIPT_HEBREW: LazyLock<Regex> = pattern!(r"\p{Script=Hebrew}");
pub static RE_SCRIPT_JAPANESE: LazyLock<Regex> =
    pattern!(r"[\p{Script=Hiragana}\p{Script=Katakana}\p{Script=Han}]");
pub static RE_SCRIPT_ARABIC_LETTER: LazyLock<Regex> =
    pattern!(r"[\p{Script=Arabic}\p{Script=Syriac}\p{Script=Thaana}\p{Script=Mandaic}]");

// ---------------------------------------------------------------- RFC 规定与上下文例外

pub static RE_VIRAMA: LazyLock<Regex> = pattern!(
    r"[\u{094d}\u{09cd}\u{0a4d}\u{0acd}\u{0b4d}\u{0bcd}\u{0c4d}\u{0ccd}\u{0d3b}\u{0d3c}\u{0d4d}\u{0dca}\u{1b44}\u{1baa}\u{1bab}\u{a9c0}\u{11046}\u{1107f}\u{110b9}\u{11133}\u{11134}\u{111c0}\u{11235}\u{1134d}\u{11442}\u{114c2}\u{115bf}\u{1163f}\u{116b6}\u{11c3f}\u{11d44}\u{11d45}]",
);
pub static RE_RFC5892_DISALLOWED: LazyLock<Regex> = pattern!(
    r"[\u{0640}\u{07fa}\u{302e}\u{302f}\u{3031}\u{3032}\u{3033}\u{3034}\u{3035}\u{303b}]",
);
pub static RE_CONTEXTO_EXCEPTIONS: LazyLock<Regex> =
    pattern!(r"[\u{00b7}\u{0375}\u{05f3}\u{05f4}\u{200c}\u{200d}\u{30fb}]");
pub static RE_PVALID_EXCEPTIONS: LazyLock<Regex> =
    pattern!(r"[\u{00df}\u{03c2}\u{06fd}\u{06fe}\u{0f0b}\u{3007}]");

// ---------------------------------------------------------------- 组合模式

// 上游：`[RE_ASCII_DIGIT, RE_EXT_ARABIC_INDIC_DIGIT].map(r => r.source).join("|")`。
pub static RE_EUROPEAN_NUMBER: LazyLock<Regex> = pattern!(r"[0-9]|[\u{06f0}-\u{06f9}]");

// 上游：`[RE_LETTER, RE_EUROPEAN_SEPARATOR, RE_COMMON_SEPARATOR, RE_NUMBER_DECIMAL,
// RE_MARK_NONSPACING, RE_MARK_SPACING_COMBINING, RE_CONTEXTO_EXCEPTIONS, RE_PVALID_EXCEPTIONS]
// .map(r => r.source).join("|")`。
pub static RE_PERMITTED_CATEGORY: LazyLock<Regex> = pattern!(
    r"\p{L}|[\u{002d}\u{002b}]|[\u{002e}\u{002c}\u{003a}\u{002f}]|\p{Nd}|\p{Mn}|\p{Mc}|[\u{00b7}\u{0375}\u{05f3}\u{05f4}\u{200c}\u{200d}\u{30fb}]|[\u{00df}\u{03c2}\u{06fd}\u{06fe}\u{0f0b}\u{3007}]",
);
