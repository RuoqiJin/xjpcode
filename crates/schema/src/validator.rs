//! 对应上游 `packages/schema/src/validator.ts`。
//!
//! `compile()`：一份 schema 编译成一个 Validator。`check` 是快的布尔路径，`errors` 说出哪里错；
//! `coerce`/`parse` 是“先纠正再检查”，`validate` 是检查加解释、不纠正。

use std::sync::Arc;

use serde_json::Value;

use crate::coerce::Coercer;
use crate::compiler::{compile_check, Compiler, Failure};
use crate::explain::Explainer;
use crate::types::{
    Check, CompileError, CompileOptions, JsonSchema, ParseResult, ValidationError,
};

/// 一份编译好的 schema。
///
/// 上游 `Validator<T>` 的泛型 `T` 是 schema 的静态类型（TypeScript 才能算出来的东西）；Rust 侧没有
/// 对应的类型级推导，值本来就是 `serde_json::Value`，因此不设泛型参数。
#[derive(Clone)]
pub struct Validator {
    check: Check,
    // 解释失败要再走一遍 schema，所以编译过的 schema 与选项要留住（上游持有引用，Rust 侧持有副本）。
    schema: Arc<JsonSchema>,
    options: Arc<CompileOptions>,
}

// 检查器是闭包，`Debug` 导不出来；只报一句它是个编译好的 validator。
impl std::fmt::Debug for Validator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Validator")
    }
}

impl Validator {
    fn new(schema: Arc<JsonSchema>, options: Arc<CompileOptions>, check: Check) -> Self {
        Self {
            check,
            schema,
            options,
        }
    }

    /// `value` 合不合格。就是一个普通函数：可以脱离 validator 传出去。
    pub fn check(&self, value: &Value) -> bool {
        (self.check)(value)
    }

    /// 把检查器本身拿走：多个任务共用同一个检查器时用它（`Send + Sync`，克隆只是加一次引用）。
    pub fn check_fn(&self) -> Check {
        self.check.clone()
    }

    /// 为什么 `value` 不合格：每个不合格的关键字一条，合格时是 `[]`。
    ///
    /// 检查通过就到此为止，所以值总是合格的那些调用方不必付解释的钱。上游把解释器缓存在 validator
    /// 上；Rust 侧每次现编译一台（见 `PORTING.md` 差异表）。返回 `Result` 的理由同 `compile()`：
    /// 解释要沿着 schema 现编子检查，编译在 Rust 里是会失败的一步。
    pub fn errors(&self, value: &Value) -> Result<Vec<ValidationError>, CompileError> {
        if self.check(value) {
            return Ok(Vec::new());
        }
        let mut explainer = Explainer::new(compiler(&self.schema, &self.options)?, &self.schema);
        explainer.errors(value)
    }

    /// `value` 自己就是合格的，就是它；否则是它的错误：`check()` 与 `errors()` 合成一个结果，不纠正。
    ///
    /// 不是 TypeBox 的 `Value.Parse`（那个会先复制、填默认值、转换、删掉未声明的属性，再检查）。
    /// 这里只检查：用在“拿 `Value.Parse` 做校验”的地方可以等价替换（没有默认值和多余属性的 schema，
    /// 那些步骤对合格值什么都不改）。要转换用带 `coerce` 的 `parse()`。
    pub fn validate(&self, value: &Value) -> Result<ParseResult, CompileError> {
        if self.check(value) {
            return Ok(ParseResult::Ok {
                value: value.clone(),
            });
        }
        Ok(ParseResult::Errors {
            errors: self.errors(value)?,
        })
    }

    /// `value` 朝 schema 纠正后的样子：数字字符串变成数字、`"true"` 变成 `true`、单个值变成一个元素的
    /// 数组、可选属性上的 `null` 被丢掉、联合取第一个转换后成立的分支（每条规则见 `coerce.ts`）。
    /// 只复制改过的路径，纠正不了的值原样留着。
    ///
    /// 上游只在 `coerce: true` 编译出来的 `CoercingValidator` 上有这个方法；Rust 没有“按选项决定
    /// 有没有这个方法”的类型，所以它一直在（见 `PORTING.md` 差异表）。纠正器每次现建，结论相同。
    pub fn coerce(&self, value: &Value) -> Result<Value, CompileError> {
        let compiler = compiler(&self.schema, &self.options)?;
        let mut coercer = Coercer::new(compiler, &self.schema);
        Ok(coercer.coerce(value))
    }

    /// 纠正 `value` 然后检查它：合格时是纠正后的值，否则是纠正后值的错误。
    pub fn parse(&self, value: &Value) -> Result<ParseResult, CompileError> {
        let coerced = self.coerce(value)?;
        self.validate(&coerced)
    }
}

/// 把 `schema` 编译成 Validator。
///
/// schema 不成形、或 `$ref` 指了不存在的东西时返回错误（上游在这里 throw；别的文档用
/// `options.schemas` 登记）。schema 只读，从不修改。
pub fn compile(schema: &JsonSchema, options: &CompileOptions) -> Result<Validator, CompileError> {
    let check = compile_check(schema, options)?;
    Ok(Validator::new(
        Arc::new(schema.clone()),
        Arc::new(options.clone()),
        check,
    ))
}

// 大多数 schema 编译时不用追踪动态作用域；有首尾配对 `$dynamicRef` 的那份要带追踪再编一次。
fn compiler<'a>(
    schema: &'a JsonSchema,
    options: &'a CompileOptions,
) -> Result<Compiler<'a>, CompileError> {
    let mut plain = Compiler::new(schema, options, false);
    match plain.root() {
        Ok(_) => Ok(plain),
        Err(Failure::Error(error)) => Err(error),
        Err(Failure::Restart) => {
            let mut dynamic = Compiler::new(schema, options, true);
            match dynamic.root() {
                Ok(_) => Ok(dynamic),
                Err(Failure::Error(error)) => Err(error),
                Err(Failure::Restart) => Err(CompileError::new("dynamic scope restart failed")),
            }
        }
    }
}
