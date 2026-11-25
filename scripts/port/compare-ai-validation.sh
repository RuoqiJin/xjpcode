#!/usr/bin/env bash
# 与 pi 逐条对账 validateToolArguments：拿 pi 的源码现跑它自己的探针测试，再用我们的实现跑同一批输入。
#
#   scripts/port/compare-ai-validation.sh
#
# 第一步用 node 22 直接跑上游 `packages/ai/test/validation-probes.test.ts`（TypeBox 1.3.27 现装），
# 把每次调用的 schema、参数与 pi 的结论记进 `ai-validation/probes.json`；随后 `cargo test` 读同一份
# 记录，按 `ok` / `REJECT` / `THROW` 的同一行文字逐条比。两边不一致时 cargo test 会把差异列出来。

set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$root"

node "$root/scripts/port/extract-validation-probes.mjs"
cargo test -p xjpcode-ai --test validation --test validation_probes

if ! git diff --quiet -- scripts/port/ai-validation/probes.json; then
	printf '\nnote: probes.json changed — pi reported something different than last time\n'
	git diff --stat -- scripts/port/ai-validation/probes.json
fi
