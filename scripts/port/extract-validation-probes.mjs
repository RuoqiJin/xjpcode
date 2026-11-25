// 把上游 `packages/ai/test/validation-probes.test.ts` 的每次 `validateToolArguments` 调用记下来，
// 写成 `scripts/port/ai-validation/probes.json`：`crates/ai/test/validation_probes.rs` 拿这份记录
// 逐条跑我们的实现，按同一个格式（`ok <参数>` / `REJECT <第一行错误>` / `THROW <第一行>`）比。
//
// 做法：把上游那份测试复制到临时目录，只改 import（TypeBox、vitest、validation），让它跑在一个极小的
// vitest 替身上；`validateToolArguments` 换成一层记录包装，把（工具名、schema、参数、pi 的结论）记下。
// schema 是 TypeBox 现搭的 JSON Schema，用 JSON 往返一次去掉符号键。
//
// 没有对应值的用例记录时略过（`PORTING.md` 差异表 #40、#51、#66、#68、#71）：
// - 结果是 bigint 的用例（`Type.BigInt()` 的 1n、12n）：Rust 侧没有 bigint；
// - 参数里有孤立代理项的字符串：`serde_json` 表示不了，Rust 的 `String` 也装不下；
// - `Undefined from JSON null`：pi 把 null 转成 undefined，JSON 里没有这个值；
// - `late`：它改同一个 schema 对象再调一次，pi 的 `WeakMap` 仍然用旧 validator；Rust 侧每次现编；
// - `mcp-invalid-regex`：pi 抛的是 V8 的正则报错，Rust 侧是 `regress` 的。
//
// 用法：
//   node scripts/port/extract-validation-probes.mjs
//
// TypeBox 1.3.27 在临时目录里现装（`TYPEBOX_ENTRY` 可以指向一份现成的
// `.../node_modules/typebox/build/index.mjs`）。pi 的源码位置默认是 ~/Projects/pi-mono，可用
// PI_SOURCE 覆盖；基准提交必须是 PORTING.md 登记的那个。

import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const BASELINE = "780bf31fcaeb252fa9b68df3964c735eae75bde3";
const TYPEBOX_VERSION = "1.3.27";
const SKIPPED = new Map([
	["late", "pi's WeakMap keeps the validator of a schema mutated later; Rust compiles per call (diff #71)"],
	["mcp-invalid-regex", "pi throws V8's regular-expression message; regress words it differently (diff #66)"],
	["Undefined from JSON null", "pi converts null to undefined; JSON has no such value (diff #68)"],
]);

const here = dirname(fileURLToPath(import.meta.url));
const outputPath = join(here, "ai-validation", "probes.json");
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

const runDirectory = mkdtempSync(join(tmpdir(), "xjpcode-validation-probes-"));
const piTest = join(piRepo, "packages", "ai", "test", "validation-probes.test.ts");
const piSource = join(piRepo, "packages", "ai", "src");
const schemaSource = join(piRepo, "packages", "schema", "src");
const source = readFileSync(piTest, "utf8");

// pi 的源码按包名 import 自己的工作区包；pi 的仓库里没有装 node_modules（对照期间保持干净），所以
// 把 `validation.ts` 复制一份，只把两个包名换成源码路径。
const validation = readFileSync(join(piSource, "utils", "validation.ts"), "utf8")
	.replace(
		'from "@earendil-works/pi-schema"',
		`from ${JSON.stringify(join(schemaSource, "index.ts"))}`,
	)
	.replace(
		'from "@earendil-works/pi-schema/formats"',
		`from ${JSON.stringify(join(schemaSource, "formats", "index.ts"))}`,
	);
if (validation.includes("@earendil-works/pi-schema") && !validation.includes("import type")) {
	fail("the upstream validation.ts imports pi-schema in a way this extractor does not rewrite");
}

// 只替换 import；测试正文原样执行。
const rewritten = source
	.replace('import { Type } from "typebox";', `import { Type } from "${typeboxEntry()}";`)
	.replace(
		'import { describe, expect, it } from "vitest";',
		'import { describe, expect, it } from "./vitest-shim.mjs";',
	)
	.replaceAll('from "../src/', `from "${piSource}/`)
	.replace(
		`from "${piSource}/utils/validation.ts"`,
		'from "./capture.mjs"',
	);

if (!rewritten.includes('from "./capture.mjs"')) {
	fail("the upstream test no longer imports validateToolArguments the way this extractor rewrites");
}

// 记录包装：结论的写法与测试里的 `outcome()` 逐字相同。
const capture = `import { validateToolArguments as real } from "./validation.ts";
import { writeFileSync } from "node:fs";

const cases = [];
const skipped = [];

export function validateToolArguments(tool, call) {
	const name = tool.name;
	const schema = tool.parameters;
	const args = call.arguments;
	const record = (outcome, result, failed) => {
		if (${JSON.stringify([...SKIPPED.keys()])}.includes(name)) {
			skipped.push({ name, reason: ${JSON.stringify([...SKIPPED.values()])}[${JSON.stringify([...SKIPPED.keys()])}.indexOf(name)] });
			return;
		}
		const notJson = [
			["schema", schema],
			["arguments", args],
			...(failed ? [] : [["result", result]]),
		]
			.filter(([, value]) => !jsonValue(value))
			.map(([part]) => part);
		if (notJson.length > 0) {
			skipped.push({ name, reason: \`not a JSON value in Rust: \${notJson.join(", ")}\` });
			return;
		}
		cases.push({
			name,
			schema: JSON.parse(JSON.stringify(schema)),
			arguments: JSON.parse(JSON.stringify(args)),
			expected: outcome,
		});
	};
	try {
		const result = real(tool, call);
		record(\`ok \${JSON.stringify(result, (_key, value) => (typeof value === "bigint" ? \`\${value}n\` : value))}\`, result, false);
		return result;
	} catch (error) {
		const message = String(error?.message ?? error);
		record(
			message.startsWith("Validation failed")
				? \`REJECT \${message.split("\\n")[1]?.trim()}\`
				: \`THROW \${message.split("\\n")[0]}\`,
			undefined,
			true,
		);
		throw error;
	}
}

function jsonValue(value, seen = new Set()) {
	if (value === null) return true;
	switch (typeof value) {
		case "boolean":
		case "undefined":
			// 对象里值为 undefined 的属性被 JSON.stringify 丢掉，只当作缺席：与 Rust 侧“没有这个键”
			// 是同一个意思（Undefined from JSON null 用到这里）。
			return true;
		case "number":
			return Number.isFinite(value);
		case "string":
			return value.isWellFormed();
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

process.on("exit", () => {
	writeFileSync(process.env.CASES_OUT, \`\${JSON.stringify({ skipped, cases }, null, 2)}\\n\`);
});
`;

const shim = `import assert from "node:assert";

export function describe(_name, fn) {
	fn();
}
export function it(_name, fn) {
	fn();
}
it.skip = () => {};

export function expect(actual) {
	return {
		toBe(expected) {
			assert.strictEqual(actual, expected);
		},
	};
}
`;

const runPath = join(runDirectory, "validation-probes.test.ts");
writeFileSync(runPath, rewritten);
writeFileSync(join(runDirectory, "validation.ts"), validation);
writeFileSync(join(runDirectory, "vitest-shim.mjs"), shim);
writeFileSync(join(runDirectory, "capture.mjs"), capture);
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
mkdirSync(dirname(outputPath), { recursive: true });
// 同一次调用可能被两条用例重复走到：内容完全相同的只留一条。
const unique = new Map();
for (const item of document.cases) unique.set(JSON.stringify(item), item);
const cases = [...unique.values()];

writeFileSync(
	outputPath,
	`${JSON.stringify(
		{
			source: "packages/ai/test/validation-probes.test.ts",
			skipped: document.skipped
				.filter((item, index, all) => all.findIndex((other) => other.name === item.name) === index)
				.sort((left, right) => (left.name < right.name ? -1 : 1)),
			cases,
		},
		null,
		2,
	)}\n`,
);
rmSync(runDirectory, { recursive: true, force: true });
for (const directory of typeboxDirectories) rmSync(directory, { recursive: true, force: true });
console.error(`wrote ${cases.length} cases to ${outputPath}`);
if (document.skipped.length > 0) {
	console.error(`skipped ${document.skipped.length} calls: ${document.skipped.map((item) => item.name).join(", ")}`);
}
