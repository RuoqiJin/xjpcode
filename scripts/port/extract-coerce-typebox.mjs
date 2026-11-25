// 把上游 `packages/schema/test/coerce-typebox.test.ts` 的每次 `Value.Convert` / `Value.Check` 调用
// 记下来，写成 `scripts/port/schema-coerce/typebox-cases.json`：
// `crates/schema/test/coerce_typebox.rs` 拿这份记录逐条跑我们的纠正器。
//
// 做法：把上游那份测试复制到临时目录，只改 import 与两个入口（`convert`、`Value.Check`），让它跑在
// 一个极小的 vitest 替身上；每次调用都把（schema、值、pi 的结果）记下。schema 是 TypeBox 现搭的
// JSON Schema，用 JSON 往返一次去掉符号键。值为 bigint、undefined、Symbol、函数或含这些的用例在 Rust
// 侧没有对应值（PORTING.md 差异表 #40），略过。
//
// 用法：
//   node scripts/port/extract-coerce-typebox.mjs
//
// TypeBox 1.3.27 在临时目录里现装（`TYPEBOX_ENTRY` 可以指向一份现成的
// `.../node_modules/typebox/build/index.mjs`）。pi 的源码位置默认是 ~/Projects/pi-mono，可用
// PI_SOURCE 覆盖；基准提交必须是 PORTING.md 登记的那个。

import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const BASELINE = "780bf31fcaeb252fa9b68df3964c735eae75bde3";
const TYPEBOX_VERSION = "1.3.27";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "../..");
const outputPath = join(here, "schema-coerce", "typebox-cases.json");
const piRepo = process.env.PI_SOURCE ?? join(homedir(), "Projects", "pi-mono");

function fail(message) {
	console.error(message);
	process.exit(1);
}

const commit = (() => {
	try {
		return execFileSync("git", ["-C", piRepo, "rev-parse", "HEAD"], { encoding: "utf8" }).trim();
	} catch (error) {
		fail(`cannot read the pi checkout at ${piRepo}: ${error.message}`);
	}
})();
if (commit !== BASELINE) {
	fail(`the pi checkout is at ${commit}, but PORTING.md registers ${BASELINE}`);
}
const status = execFileSync("git", ["-C", piRepo, "status", "--porcelain"], { encoding: "utf8" }).trim();
if (status !== "") {
	fail(`the pi checkout has uncommitted changes:\n${status}`);
}

// TypeBox 的 ESM 入口：环境变量优先，否则装到临时目录。
const typeboxDirectories = [];
function typeboxEntry() {
	if (process.env.TYPEBOX_ENTRY !== undefined) return process.env.TYPEBOX_ENTRY;
	const directory = mkdtempSync(join(tmpdir(), "xjpcode-typebox-"));
	typeboxDirectories.push(directory);
	try {
		execFileSync(
			"npm",
			["install", `typebox@${TYPEBOX_VERSION}`, "--prefix", directory, "--no-audit", "--no-fund", "--silent"],
			{ stdio: ["ignore", "ignore", "inherit"] },
		);
	} catch (error) {
		fail(`cannot install typebox@${TYPEBOX_VERSION}: ${error.message}`);
	}
	return join(directory, "node_modules", "typebox", "build", "index.mjs");
}

const runDirectory = mkdtempSync(join(tmpdir(), "xjpcode-coerce-typebox-"));
const typescriptTest = join(piRepo, "packages", "schema", "test", "coerce-typebox.test.ts");
const source = readFileSync(typescriptTest, "utf8");

// 只替换 import 与两个入口；测试正文原样执行。
const rewritten = source
	.replace('import { Type } from "typebox";', `import { Type } from "${typeboxEntry()}";`)
	.replace(
		'import { describe, expect, it } from "vitest";',
		'import { describe, expect, it, recordConvert, recordCheck } from "./vitest-shim.mjs";',
	)
	.replaceAll('from "../src/', `from "${join(piRepo, "packages", "schema", "src")}/`)
	.replace(
		"return new Coercer(new Compiler(schema as object, { formats }, false), schema).convertOnly(value);",
		"return recordConvert(schema, value, () => new Coercer(new Compiler(schema as object, { formats }, false), schema).convertOnly(value));",
	)
	.replace(
		"Check: (schema: unknown, value: unknown) => compile(schema as object, { formats }).check(value),",
		"Check: (schema: unknown, value: unknown) => recordCheck(schema, value, () => compile(schema as object, { formats }).check(value)),",
	);

if (!rewritten.includes("recordConvert(") || !rewritten.includes("recordCheck(")) {
	fail("the upstream test no longer has the two entry points this extractor rewrites");
}

const shim = `import assert from "node:assert";
import { writeFileSync } from "node:fs";

const cases = [];
let group = "";
let title = "";
let count = 0;

export function describe(name, fn) {
	group = name;
	fn();
}
export function it(name, fn) {
	title = name;
	fn();
}
it.skip = () => {};

export function recordConvert(schema, value, run) {
	const result = run();
	record("convert", schema, value, result);
	return result;
}

export function recordCheck(schema, value, run) {
	const result = run();
	record("check", schema, value, result);
	return result;
}

function record(kind, schema, value, expected) {
	if (!jsonValue(schema) || !jsonValue(value) || !jsonValue(expected)) return;
	cases.push({ kind, group, name: \`\${title} #\${++count}\`, schema, value, expected });
}

function jsonValue(value, seen = new Set()) {
	if (value === null) return true;
	switch (typeof value) {
		case "string":
		case "boolean":
			return true;
		case "number":
			return Number.isFinite(value);
		case "object":
			break;
		default:
			return false;
	}
	if (seen.has(value)) return false;
	seen.add(value);
	if (Array.isArray(value)) {
		const ok = value.every((item) => jsonValue(item, seen));
		seen.delete(value);
		return ok;
	}
	const proto = Object.getPrototypeOf(value);
	if (proto !== Object.prototype && proto !== null) return false;
	const ok = Object.values(value).every((item) => jsonValue(item, seen));
	seen.delete(value);
	return ok;
}

export function expect(actual) {
	return {
		toEqual(expected) {
			assert.deepStrictEqual(actual, expected);
		},
		toBe(expected) {
			assert.deepStrictEqual(actual, expected);
		},
	};
}
export const expectTypeOf = () => new Proxy({}, { get: () => () => {} });

process.on("exit", () => {
	writeFileSync(
		process.env.CASES_OUT,
		\`\${JSON.stringify({ source: "packages/schema/test/coerce-typebox.test.ts", cases }, null, 2)}\\n\`,
	);
	console.error(\`recorded \${cases.length} cases\`);
});
`;

const runPath = join(runDirectory, "coerce-typebox.test.ts");
writeFileSync(runPath, rewritten);
writeFileSync(join(runDirectory, "vitest-shim.mjs"), shim);
const casesOut = join(runDirectory, "cases.json");
try {
	execFileSync(process.execPath, [runPath], {
		cwd: runDirectory,
		env: { ...process.env, CASES_OUT: casesOut },
		stdio: ["ignore", "inherit", "inherit"],
	});
} catch (error) {
	fail(`running the upstream test failed: ${error.message}`);
}
const document = JSON.parse(readFileSync(casesOut, "utf8"));
writeFileSync(outputPath, `${JSON.stringify(document, null, 2)}\n`);
rmSync(runDirectory, { recursive: true, force: true });
for (const directory of typeboxDirectories) rmSync(directory, { recursive: true, force: true });
console.error(`wrote ${document.cases.length} cases to ${outputPath}`);
