#!/usr/bin/env bash
# 自检：一条命令跑完 PORTING.md 约定里列的那几样。验收时跑的也是它。
#
#   scripts/port/check.sh
#
# pi 的源码位置默认是 ~/Projects/pi-mono，可用 PI_SOURCE 覆盖。

set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$root"

pi_source=${PI_SOURCE:-$HOME/Projects/pi-mono}

# 辅助模块：不是测试目标，由用到的测试按模块引入，所以不登记 `[[test]]`。
helpers=(
	"crates/schema/test/suite.rs"
	"crates/agent/test/utils/calculate.rs"
	"crates/agent/test/utils/get_current_time.rs"
	"crates/agent/test/utils/tools.rs"
	"crates/agent/test/utils/wait_for_tick.rs"
)

fail() {
	printf 'check: %s\n' "$1" >&2
	exit 1
}

# 跑一条命令：失败就把它的话原样打出来；成功但打了警告也算没过。
run() {
	local name=$1
	shift
	local output
	if ! output=$("$@" 2>&1); then
		printf '%s\n' "$output"
		fail "$name failed"
	fi
	if printf '%s\n' "$output" | grep -q '^warning'; then
		printf '%s\n' "$output"
		fail "$name printed warnings"
	fi
	printf 'ok   %s\n' "$name"
}

# 1. 构建与各包的测试：全部通过，且没有警告。根包（界面）也在内。
run "cargo build --workspace --all-targets" cargo build --workspace --all-targets
run "cargo test --workspace" cargo test --workspace

# 2. `test/` 下的每个测试文件都在 Cargo.toml 里登记了。
#    Cargo 只自动发现 `tests/`，我们的目录叫 `test/`，漏登记的文件会静默不参与测试。
for manifest in crates/*/Cargo.toml; do
	crate=${manifest%/Cargo.toml}
	for file in "$crate"/test/*.rs; do
		[ -e "$file" ] || continue
		relative="test/$(basename "$file")"
		helper=no
		for known in "${helpers[@]}"; do
			if [ "$crate/$relative" = "$known" ]; then
				helper=yes
			fi
		done
		if [ "$helper" = yes ]; then
			continue
		fi
		if ! sed -n 's/^path = "\(test\/[^"]*\)"$/\1/p' "$manifest" | grep -qxF "$relative"; then
			fail "$crate/$relative is not registered as [[test]] in $manifest"
		fi
	done
done
printf 'ok   every test/ file is registered\n'

# 3. 旧核心已经删掉，界面接在新内核上。
#    这一项以前查的是 `src/`、`tests/`、`build.rs` 与 master 一致；这一轮搬完界面、删掉旧核心，
#    改成查"旧核心不在、也没有残留引用、根包依赖新内核"。
for path in \
	src/engine \
	src/session \
	src/services \
	src/server \
	src/transcript.rs \
	src/compact.rs \
	src/bootstrap.rs \
	src/content_parser.rs \
	src/constants \
	src/utils \
	src/types/message.rs \
	src/types/llm.rs; do
	if [ -e "$path" ]; then
		fail "old core is still present: $path"
	fi
done
if grep -Rn --include=*.rs -E 'crate::(engine|session|services|server)\b|xjpcode::(engine|session|services|server)\b' src tests; then
	fail "src/ or tests/ still reference the deleted core"
fi
grep -q 'xjpcode-coding-agent' Cargo.toml || fail "the root package is not wired to xjpcode-coding-agent"
grep -q 'session_setup' src/main.rs || fail "the binary does not build its session through src/session_setup.rs"
printf 'ok   the old core is gone and the binary runs on the new kernel\n'

# 4. pi 的源码仓库是干净的：基准提交里的源码才是这次对照的样子。
[ -d "$pi_source/.git" ] || fail "no pi checkout at $pi_source (set PI_SOURCE)"
dirty=$(git -C "$pi_source" status --porcelain) || fail "cannot read $pi_source"
if [ -n "$dirty" ]; then
	fail "the pi checkout at $pi_source has uncommitted changes:
$dirty"
fi
printf 'ok   the pi checkout is clean\n'

# 5. 引用了已搬 pi 模块的测试都有着落（搬了、搬了一部分、或写明为什么没搬）。
PI_SOURCE="$pi_source" python3 scripts/port/check-pi-tests.py || fail "pi test accounting failed"

printf '\ncheck: all good\n'
