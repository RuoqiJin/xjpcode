#!/usr/bin/env bash
# 与 pi 逐条对账：拿 pi 的源码现算一遍我们的输入，再用我们的实现跑同一批输入。
#
#   scripts/port/compare-schema-errors.sh
#
# 两批输入：`schema-errors/cases.json`（手写用例）与 `test/fixtures` 里的官方 JSON-Schema-Test-Suite。
# 前两步用 node 22 直接跑 pi 的 `.ts`，结果写回 `pi.json` 与 `suite-pi.json`；后一步是 `cargo test`，
# 读的也是这两个文件。两边不一致时 `cargo test` 会把每个用例的差异列出来。

set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$root"

node "$root/scripts/port/compare-schema-errors.mjs" --update
node "$root/scripts/port/compare-schema-errors.mjs" --suite --update
cargo test -p xjpcode-schema --test port_compare

for recorded in pi.json suite-pi.json; do
	if ! git diff --quiet -- "scripts/port/schema-errors/$recorded"; then
		printf '\nnote: %s changed — pi reported something different than last time\n' "$recorded"
		git diff --stat -- "scripts/port/schema-errors/$recorded"
	fi
done
