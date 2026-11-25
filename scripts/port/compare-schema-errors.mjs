// 用 pi 自己的实现跑我们的输入，把每个用例的 check 与 errors 记下来；crates/schema/test/port_compare.rs
// 读同一批输入、同一份记录，逐条比。
//
// 两批输入：
//   （默认）scripts/port/schema-errors/cases.json —— 手写的用例，覆盖每个关键字与每种消息写法
//   --suite      crates/schema/test/fixtures 里的官方 JSON-Schema-Test-Suite（与一致性测试同一批）
//
// 用法：
//   node scripts/port/compare-schema-errors.mjs                 # 现算一遍，跟记下的逐条比
//   node scripts/port/compare-schema-errors.mjs --update        # 现算一遍，写回 pi.json
//   node scripts/port/compare-schema-errors.mjs --suite         # 同一批一致性用例
//   node scripts/port/compare-schema-errors.mjs --suite --update  # 写回 suite-pi.json
//
// pi 的源码位置默认是 ~/Projects/pi-mono，可用 PI_SOURCE 覆盖。基准提交必须是 PORTING.md 登记的那个：
// 换了提交，记下来的东西就不再是同一个东西的输出。

import { execFileSync } from "node:child_process";
import { readFileSync, readdirSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, relative as relativePath, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const BASELINE = "780bf31fcaeb252fa9b68df3964c735eae75bde3";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "../..");
const casesPath = join(here, "schema-errors", "cases.json");
const recordedPath = join(here, "schema-errors", "pi.json");
const suiteRecordedPath = join(here, "schema-errors", "suite-pi.json");
const fixtures = join(repo, "crates", "schema", "test", "fixtures");
const piRepo = process.env.PI_SOURCE ?? join(homedir(), "Projects", "pi-mono");
const update = process.argv.includes("--update");
const suite = process.argv.includes("--suite");

function relative(path) {
	return relativePath(repo, path);
}

function head(repo) {
	try {
		return execFileSync("git", ["-C", repo, "rev-parse", "HEAD"], { encoding: "utf8" }).trim();
	} catch (error) {
		fail(`cannot read the pi checkout at ${repo}: ${error.message}`);
	}
}

function fail(message) {
	console.error(message);
	process.exit(1);
}

const commit = head(piRepo);
if (commit !== BASELINE) {
	fail(`the pi checkout is at ${commit}, but PORTING.md registers ${BASELINE}`);
}
const status = execFileSync("git", ["-C", piRepo, "status", "--porcelain"], { encoding: "utf8" }).trim();
if (status !== "") {
	fail(`the pi checkout has uncommitted changes:\n${status}`);
}

const source = join(piRepo, "packages", "schema", "src");
const { compile } = await import(pathToFileURL(join(source, "index.ts")).href);
const { formats } = await import(pathToFileURL(join(source, "formats", "index.ts")).href);

function optionsOf(given) {
	const options = {};
	if (given.draft === "7") options.draft = "7";
	if (given.formats === true) options.formats = formats;
	else if (given.formats !== undefined) options.formats = given.formats;
	if (given.unresolvedRefs !== undefined) options.unresolvedRefs = given.unresolvedRefs;
	if (given.schemas !== undefined) options.schemas = given.schemas;
	return options;
}

// 一次 compile，三种结果：schema 本身写坏时 compile 抛错（记下它的话，两边比这句话）；pi 的 errors()
// 在环上会抛 RangeError，记下来当“没比”。
function explain(schema, value, options) {
	let validator;
	try {
		validator = compile(schema, optionsOf(options ?? {}));
	} catch (error) {
		return { compileError: String(error?.message ?? error) };
	}
	const check = validator.check(value);
	try {
		return { check, errors: validator.errors(value) };
	} catch (error) {
		return { check, errors: null, threw: String(error) };
	}
}

function readJson(path) {
	return JSON.parse(readFileSync(path, "utf8"));
}

function jsonFiles(directory) {
	const out = [];
	for (const entry of readdirSync(directory, { withFileTypes: true }).sort((a, b) =>
		a.name < b.name ? -1 : a.name > b.name ? 1 : 0,
	)) {
		const path = join(directory, entry.name);
		if (entry.isDirectory()) out.push(...jsonFiles(path));
		else if (entry.name.endsWith(".json")) out.push(path);
	}
	return out;
}

// 与 crates/schema/test/suite.rs 的 suite_schemas() 一一对应。
function suiteSchemas() {
	const root = join(fixtures, "json-schema-test-suite");
	const schemas = {};
	for (const path of jsonFiles(join(root, "remotes"))) {
		const name = path.slice(join(root, "remotes").length + 1).replaceAll("\\", "/");
		schemas[`http://localhost:1234/${name}`] = readJson(path);
	}
	const meta = join(fixtures, "meta-schemas");
	schemas["http://json-schema.org/draft-07/schema"] = readJson(join(meta, "draft-07-schema.json"));
	schemas["https://json-schema.org/draft/2020-12/schema"] = readJson(
		join(meta, "draft2020-12-schema.json"),
	);
	for (const vocabulary of [
		"core",
		"applicator",
		"unevaluated",
		"validation",
		"meta-data",
		"format-annotation",
		"content",
	]) {
		schemas[`https://json-schema.org/draft/2020-12/meta/${vocabulary}`] = readJson(
			join(meta, `draft2020-12-meta-${vocabulary}.json`),
		);
	}
	return schemas;
}

// 与 crates/schema/test/suite.rs 的 suite_files() 一一对应：drafts 2020-12 与 7，文件按路径排序。
function suiteCases() {
	const root = join(fixtures, "json-schema-test-suite", "tests");
	const schemas = suiteSchemas();
	const out = [];
	for (const directory of ["draft2020-12", "draft7"]) {
		for (const path of jsonFiles(join(root, directory))) {
			const name = path
				.slice(root.length + 1)
				.replaceAll("\\", "/")
				.replace(/\.json$/, "");
			const draft = directory === "draft7" ? "7" : "2020-12";
			// format 测试是在断言 format；别处 format 是注解，与 draft 2020-12 的规定一致。
			const options = {
				draft,
				schemas,
				formats: name.includes("/optional/format/") ? formats : undefined,
			};
			for (const group of readJson(path)) {
				const validator = compile(group.schema, optionsOf(options));
				for (const test of group.tests) {
					const key = `${name} > ${group.description} > ${test.description}`;
					const check = validator.check(test.data);
					try {
						out.push({ name: key, check, errors: validator.errors(test.data) });
					} catch (error) {
						out.push({ name: key, check, errors: null, threw: String(error) });
					}
				}
			}
		}
	}
	return out;
}

const inputLabel = suite ? "test/fixtures (the official suite)" : relative(casesPath);
const outputPath = suite ? suiteRecordedPath : recordedPath;
const results = suite
	? suiteCases()
	: readJson(casesPath).map((item) => {
			const result = explain(item.schema, item.value, item.options);
			return { name: item.name, ...result };
		});

const text = `${JSON.stringify(results, null, 2)}\n`;

if (update) {
	writeFileSync(outputPath, text);
	console.error(`wrote ${results.length} cases to ${outputPath}`);
	process.exit(0);
}

const recorded = readJson(outputPath);
const differences = [];
if (recorded.length !== results.length) {
	differences.push(
		`${relative(outputPath)} has ${recorded.length} cases, ${inputLabel} has ${results.length}`,
	);
}
for (const [index, result] of results.entries()) {
	const known = recorded[index];
	if (known === undefined) continue;
	const expected = JSON.stringify({ check: known.check, errors: known.errors, compileError: known.compileError });
	const actual = JSON.stringify({ check: result.check, errors: result.errors, compileError: result.compileError });
	if (expected !== actual) {
		differences.push([result.name, `  recorded: ${expected}`, `  now:      ${actual}`].join("\n"));
	}
}
const threw = results.filter((result) => result.errors === null).length;
if (threw > 0) {
	console.error(`note: pi threw on ${threw} case(s); port_compare skips those`);
}

if (differences.length > 0) {
	fail(`pi's own output moved for ${differences.length} case(s); run with --update:\n\n${differences.join("\n\n")}`);
}
console.error(`pi agrees with ${relative(outputPath)} on all ${results.length} cases`);
