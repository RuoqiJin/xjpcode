// 用 pi 自己的 `StreamingJsonParser` 跑同一批输入，把每一段之后的快照与改动记下来；
// `crates/ai/test/port_compare_streaming_json.rs` 拿这份记录逐条跑我们的实现，按
// `JSON.stringify` 的文本比（键序、数字写法都在内，见 `PORTING.md` 差异表 #62、#64）。
//
// 两批输入：
//   - `scripts/port/ai-streaming-json/cases.json`：手写用例，覆盖每条读取规矩、每处已知差异；
//   - 上游的 `test/data/json-test-suite.json`（JSONTestSuite，316 份）与同一套随机文档生成器
//     生成的文档（与 `streaming-json.test.ts` 的种子同式）。
//
// pi 里没有对应值的输入（`Infinity` 之类的大数、孤立代理项、被分段拆开的原始代理对）在记录时
// 略过，原因写进 `skipped`；Rust 侧那几样由 `streaming_json.rs` 的用例守着（差异表 #101、#104）。
//
// 做法：把 pi 的 `streaming-json.ts`、`json-parse.ts` 复制到临时目录（内容不动），旁边放一个
// `partial-json` 的替身（`parseStreamingJson()` 才用得到它，`StreamingJsonParser` 不碰），
// 用 node 22 直接跑 `.ts`。
//
// 用法：
//   node scripts/port/compare-ai-streaming-json.mjs            # 现算一遍，跟记下的逐条比
//   node scripts/port/compare-ai-streaming-json.mjs --update   # 现算一遍，写回记录
//
// pi 的源码位置默认是 ~/Projects/pi-mono，可用 PI_SOURCE 覆盖；基准提交必须是 `PORTING.md`
// 登记的那个。

import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const BASELINE = "780bf31fcaeb252fa9b68df3964c735eae75bde3";
const CASE_CHUNKS = [1, 2, 3, 7, "whole"];
const CORPUS_CHUNKS = [1, 3, "whole"];

const here = dirname(fileURLToPath(import.meta.url));
const repo = join(here, "../..");
const casesPath = join(here, "ai-streaming-json", "cases.json");
const casesRecordedPath = join(here, "ai-streaming-json", "pi.json");
const corpusRecordedPath = join(here, "ai-streaming-json", "suite-pi.json");
const suitePath = join(repo, "crates", "ai", "test", "data", "json-test-suite.json");
const piRepo = process.env.PI_SOURCE ?? join(homedir(), "Projects", "pi-mono");
const update = process.argv.includes("--update");

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

// pi 的源码原样复制到临时目录，只补一个 `partial-json` 的替身（`json-parse.ts` 的顶层 import）。
const temp = mkdtempSync(join(tmpdir(), "pi-streaming-json-"));
const utils = join(piRepo, "packages", "ai", "src", "utils");
copyFileSync(join(utils, "streaming-json.ts"), join(temp, "streaming-json.ts"));
copyFileSync(join(utils, "json-parse.ts"), join(temp, "json-parse.ts"));
mkdirSync(join(temp, "node_modules", "partial-json"), { recursive: true });
writeFileSync(
	join(temp, "node_modules", "partial-json", "package.json"),
	JSON.stringify({ name: "partial-json", version: "0.1.7", type: "module", exports: "./index.mjs" }),
);
writeFileSync(
	join(temp, "node_modules", "partial-json", "index.mjs"),
	"export function parse() {\n\tthrow new Error(\"partial-json is never called by StreamingJsonParser\");\n}\n",
);
let StreamingJsonParser;
try {
	({ StreamingJsonParser } = await import(pathToFileURL(join(temp, "streaming-json.ts")).href));
} finally {
	rmSync(temp, { recursive: true, force: true });
}

// ---------------------------------------------------------------------------
// 记录：值必须能用 JSON 文本交出来；Rust 侧装不下的那些输入跳过。
// ---------------------------------------------------------------------------

/** 一份值在 Rust 侧表示得出来：数字有限、字符串是完整的 UTF-8。 */
function representable(value) {
	if (value === null || typeof value === "boolean") return true;
	if (typeof value === "number") return Number.isFinite(value);
	if (typeof value === "string") return Buffer.from(value, "utf8").toString("utf8") === value;
	if (Array.isArray(value)) return value.every(representable);
	if (typeof value === "object") {
		return Object.entries(value).every(([key, item]) => representable(key) && representable(item));
	}
	return false;
}

/** 文字里容器最多套几层（字符串里的括号不算）。 */
function containerDepth(text) {
	let depth = 0;
	let max = 0;
	let inString = false;
	let escape = false;
	for (const char of text) {
		if (inString) {
			if (escape) escape = false;
			else if (char === "\\") escape = true;
			else if (char === '"') inString = false;
			continue;
		}
		if (char === '"') inString = true;
		else if (char === "{" || char === "[") {
			depth += 1;
			if (depth > max) max = depth;
		} else if (char === "}" || char === "]") depth = Math.max(0, depth - 1);
	}
	return max;
}

/** 一段文字在 Rust 侧表示得出来（没有孤立的代理项）。 */
function wellFormed(piece) {
	return Buffer.from(piece, "utf8").toString("utf8") === piece;
}

function recordChanges(changes) {
	const recorded = [];
	for (const change of changes) {
		if (!representable(change.path)) return undefined;
		if (change.kind === "set") {
			if (!representable(change.value)) return undefined;
			recorded.push({ kind: "set", path: change.path, value: JSON.stringify(change.value) });
		} else {
			if (!representable(change.text)) return undefined;
			recorded.push({ kind: "append", path: change.path, text: change.text });
		}
	}
	return recorded;
}

/**
 * 把 `text` 按 `chunk` 段喂给 pi 的解析器，记下每一段的输入、快照与改动。表示不出来的返回
 * `{ skipped: reason }`。
 */
function recordRun(text, chunk, options) {
	// `serde_json` 的值最多 127 层容器（差异表 #102）；比这深的由我们的用例守着。
	if (containerDepth(text) > 127) return { skipped: "deeper than serde_json's recursion limit" };
	const parser = new StreamingJsonParser();
	const pieces = [];
	const steps = [];
	const snapshots = [];
	for (let index = 0; index < text.length; ) {
		const size = chunk === "whole" ? text.length : chunk;
		const piece = text.slice(index, index + size);
		index += size;
		// 拆开代理对的那一段记不下来（记下来就不是 Rust 装得下的文字了），只记原因。
		if (!wellFormed(piece)) return { skipped: "a piece splits a surrogate pair" };
		pieces.push(piece);
		const changes = parser.append(piece);
		const snapshot = parser.snapshot();
		if (snapshot !== undefined && !representable(snapshot)) {
			return { skipped: "a snapshot holds a value Rust cannot represent", pieces };
		}
		const recordedChanges = options.changes ? recordChanges(changes) : [];
		if (recordedChanges === undefined) {
			return { skipped: "a change holds a value Rust cannot represent", pieces };
		}
		snapshots.push(snapshot === undefined ? null : JSON.stringify(snapshot));
		if (options.changes) steps.push({ snapshot: snapshot === undefined ? null : JSON.stringify(snapshot), changes: recordedChanges });
	}
	return {
		run: options.changes
			? { chunk, pieces, steps }
			: { chunk, pieces, snapshots },
	};
}

// ---------------------------------------------------------------------------
// 输入
// ---------------------------------------------------------------------------

/** 上游 `streaming-json.test.ts` 的文档生成器（同一条 LCG、同一套求值顺序）。 */
function random(seed) {
	let state = seed;
	return () => {
		state = (state * 1103515245 + 12345) & 0x7fffffff;
		return state / 0x7fffffff;
	};
}

const pick = (rand, items) => items[Math.floor(rand() * items.length)];
const STRING_PIECES = ["a", "b", "x y", " ", "\n", "\t", "\\", '"', "/", "é", "😀", "\u0001", "\u001f", "\u2028", "{", "]", ","];

function randomString(rand) {
	let text = "";
	const length = Math.floor(rand() * 6);
	for (let index = 0; index < length; index++) text += pick(rand, STRING_PIECES);
	return text;
}

function randomScalar(rand) {
	return pick(rand, [
		() => randomString(rand),
		() => Math.floor(rand() * 2000) - 1000,
		() => rand() * 100,
		() => 1e21,
		() => -5e-8,
		() => -0,
		() => 0,
		() => true,
		() => false,
		() => null,
	])();
}

function randomValue(rand, depth) {
	const roll = rand();
	if (depth > 3 || roll < 0.3) return randomScalar(rand);
	if (roll < 0.65) {
		const members = {};
		const count = Math.floor(rand() * 4);
		for (let index = 0; index < count; index++) members[randomString(rand)] = randomValue(rand, depth + 1);
		return members;
	}
	return Array.from({ length: Math.floor(rand() * 4) }, () => randomValue(rand, depth + 1));
}

function render(value, rand, spaced) {
	const space = () => (spaced ? pick(rand, ["", "", " ", "\n  ", "\t", "\r\n"]) : "");
	if (Array.isArray(value)) {
		return `[${space()}${value.map((item) => render(item, rand, spaced)).join(`${space()},${space()}`)}${space()}]`;
	}
	if (value !== null && typeof value === "object") {
		const members = Object.entries(value).map(
			([key, item]) => `${JSON.stringify(key)}${space()}:${space()}${render(item, rand, spaced)}`,
		);
		return `{${space()}${members.join(`${space()},${space()}`)}${space()}}`;
	}
	return Object.is(value, -0) ? "-0" : JSON.stringify(value);
}

function randomDocuments(seed, count, spaced) {
	const rand = random(seed);
	return Array.from({ length: count }, () =>
		render(rand() < 0.8 ? { path: randomString(rand), args: randomValue(rand, 0) } : randomValue(rand, 0), rand, spaced),
	);
}

/** JSONTestSuite 的文档，解出文字。 */
function suiteDocuments() {
	const suite = JSON.parse(readFileSync(suitePath, "utf8"));
	return Object.entries(suite.files).map(([name, base64]) => ({
		name,
		text: Buffer.from(base64, "base64").toString("utf8"),
	}));
}

// ---------------------------------------------------------------------------
// 跑一遍，写回或比对
// ---------------------------------------------------------------------------

function recordCases() {
	const cases = JSON.parse(readFileSync(casesPath, "utf8"));
	const recorded = { baseline: BASELINE, cases: [], skipped: [] };
	for (const entry of cases) {
		const runs = [];
		for (const chunk of CASE_CHUNKS) {
			const outcome = recordRun(entry.text, chunk, { changes: true });
			if (outcome.skipped !== undefined) {
				recorded.skipped.push({ case: entry.name, chunk, reason: outcome.skipped, ...(outcome.pieces === undefined ? {} : { pieces: outcome.pieces }) });
			} else {
				runs.push(outcome.run);
			}
		}
		recorded.cases.push({ name: entry.name, text: entry.text, runs });
	}
	return recorded;
}

function recordCorpus() {
	const documents = [];
	const skipped = [];
	const add = (name, text) => {
		const runs = [];
		for (const chunk of CORPUS_CHUNKS) {
			const outcome = recordRun(text, chunk, { changes: false });
			if (outcome.skipped !== undefined) skipped.push({ document: name, chunk, reason: outcome.skipped, ...(outcome.pieces === undefined ? {} : { pieces: outcome.pieces }) });
			else runs.push(outcome.run);
		}
		documents.push({ name, text, runs });
	};
	for (const document of suiteDocuments()) add(document.name, document.text);
	for (const [seed, count, spaced] of [
		[11, 60, true],
		[12, 60, false],
		[13, 40, true],
	]) {
		randomDocuments(seed, count, spaced).forEach((text, index) => add(`random-${seed}-${index}`, text));
	}
	return { baseline: BASELINE, documents, skipped };
}

function report(path, recorded) {
	const text = `${JSON.stringify(recorded, null, "\t")}\n`;
	if (update) {
		writeFileSync(path, text);
		console.log(`wrote ${path}`);
		return;
	}
	const previous = readFileSync(path, "utf8");
	if (previous !== text) {
		console.error(`${path} is out of date; rerun with --update and check the diff`);
		process.exit(1);
	}
	console.log(`${path} is current`);
}

report(casesRecordedPath, recordCases());
report(corpusRecordedPath, recordCorpus());
