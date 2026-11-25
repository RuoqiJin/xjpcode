#!/usr/bin/env bash
# 与 pi 逐条对账 coerce() 与 parse()：拿 pi 的源码现算一遍我们的输入，再用我们的实现跑同一批输入。
#
#   scripts/port/compare-schema-coerce.sh
#
# 两批输入：`schema-coerce/cases.json`（手写用例）与 `test/fixtures` 里的官方 JSON-Schema-Test-Suite。
# 前两步用 node 22 直接跑 pi 的 `.ts`，结果写回 `pi.json` 与 `suite-pi.json`；随后重新抽取上游那份
# TypeBox `Value.Convert` 测试（`typebox-cases.json`）；最后是 `cargo test`，读的也是这几份记录。
# 两边不一致时 `cargo test` 会把每个用例的差异列出来。

set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$root"

node "$root/scripts/port/compare-schema-coerce.mjs" --update
node "$root/scripts/port/compare-schema-coerce.mjs" --suite --update
node "$root/scripts/port/extract-coerce-typebox.mjs"
cargo test -p xjpcode-schema --test port_compare_coerce --test coerce --test coerce_typebox

for recorded in pi.json suite-pi.json typebox-cases.json; do
	if ! git diff --quiet -- "scripts/port/schema-coerce/$recorded"; then
		printf '\nnote: %s changed — pi reported something different than last time\n' "$recorded"
		git diff --stat -- "scripts/port/schema-coerce/$recorded"
	fi
done
