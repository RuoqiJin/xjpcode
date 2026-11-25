#!/usr/bin/env bash
# 与 pi 逐条对账 `StreamingJsonParser`：拿 pi 的源码现算一遍同一批输入，再用我们的实现跑同一批输入。
#
#   scripts/port/compare-ai-streaming-json.sh
#
# 两批输入：`ai-streaming-json/cases.json`（手写用例，含每一段的改动）与 JSONTestSuite + 随机文档
# （`suite-pi.json`）。前两步用 node 22 直接跑 pi 的 `.ts`，结果写回 `pi.json` 与 `suite-pi.json`；
# 后一步是 `cargo test`，读的也是这两个文件。两边不一致时 `cargo test` 会把每个用例的差异列出来。

set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$root"

node "$root/scripts/port/compare-ai-streaming-json.mjs" --update
cargo test -p xjpcode-ai --test port_compare_streaming_json

for recorded in pi.json suite-pi.json; do
	if ! git diff --quiet -- "scripts/port/ai-streaming-json/$recorded"; then
		printf '\nnote: %s changed — pi reported something different last time\n' "$recorded"
		git diff --stat -- "scripts/port/ai-streaming-json/$recorded"
	fi
done
