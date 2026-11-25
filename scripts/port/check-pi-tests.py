#!/usr/bin/env python3
"""对账：pi 里引用了已搬模块的测试，是不是都有着落。

    scripts/port/check-pi-tests.py [--pi ~/Projects/pi-mono]

输入：
  scripts/port/pi-feature-sources.txt  已经搬过的 pi 源文件
  scripts/port/pi-test-accounting.tsv  每个相关 pi 测试的去处

TSV 每行三列（`#` 开头是注释）：

    <pi 测试路径>\t<ported|partial|deferred>\t<说明>

  ported    说明是搬过来的 Rust 测试文件（逗号分隔；文件必须存在）
  partial   搬了一部分；说明写成 `<Rust 测试文件> | <没搬的用例>`
  deferred  没搬；说明写原因

检查两件事：
  1. 引用了已搬模块的 pi 测试都在表里（没有"还没着落"的）。
  2. 表里的每条都还对得上：路径还在，ported/partial 指到的测试文件存在。

退出码 0 表示对上了；1 表示有缺口，缺口逐条打出来。
"""

import argparse
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
TEST_ROOTS = [
    "packages/ai/test",
    "packages/agent/test",
    "packages/coding-agent/test",
    "packages/schema/test",
]
IMPORT_RE = re.compile(r"""from\s+["']([^"']+)["']|import\s*\(\s*["']([^"']+)["']\s*\)""")


def read_list(path):
    entries = []
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if line and not line.startswith("#"):
                entries.append(line)
    return entries


def read_accounting(path):
    rows = {}
    duplicates = []
    with open(path, encoding="utf-8") as handle:
        for number, line in enumerate(handle, 1):
            line = line.rstrip("\n")
            if not line.strip() or line.lstrip().startswith("#"):
                continue
            parts = line.split("\t")
            if len(parts) != 3 or parts[1] not in {"ported", "partial", "deferred"}:
                raise SystemExit(f"{path}:{number}: expected '<path>\\t<ported|partial|deferred>\\t<detail>'")
            if parts[0] in rows:
                duplicates.append(parts[0])
            rows[parts[0]] = (parts[1], parts[2])
    return rows, duplicates


def resolves_to_ported(target, ported):
    candidates = [target, target + ".ts", os.path.join(target, "index.ts")]
    return any(candidate in ported for candidate in candidates)


def referencing_tests(pi_root, ported):
    found = {}
    for test_root in TEST_ROOTS:
        base = os.path.join(pi_root, test_root)
        if not os.path.isdir(base):
            continue
        for dirpath, _, files in os.walk(base):
            for name in files:
                if not name.endswith(".test.ts"):
                    continue
                path = os.path.join(dirpath, name)
                relative = os.path.relpath(path, pi_root)
                text = open(path, encoding="utf-8").read()
                refs = set()
                for match in IMPORT_RE.finditer(text):
                    specifier = match.group(1) or match.group(2)
                    if not specifier.startswith("."):
                        continue
                    target = os.path.normpath(os.path.join(os.path.dirname(relative), specifier))
                    if resolves_to_ported(target, ported):
                        refs.add(target)
                if refs:
                    found[relative] = sorted(refs)
    return found


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--pi", default=os.environ.get("PI_SOURCE", os.path.expanduser("~/Projects/pi-mono")))
    args = parser.parse_args()

    ported = {
        os.path.normpath(entry)
        for entry in read_list(os.path.join(ROOT, "scripts/port/pi-feature-sources.txt"))
    }
    accounting_path = os.path.join(ROOT, "scripts/port/pi-test-accounting.tsv")
    accounting, duplicates = read_accounting(accounting_path)
    candidates = referencing_tests(args.pi, ported)

    problems = []
    for path in duplicates:
        problems.append(f"duplicate accounting row: {path}")
    for path in sorted(candidates):
        if path not in accounting:
            referenced = ", ".join(os.path.basename(ref) for ref in candidates[path])
            problems.append(f"unaccounted pi test: {path} (references {referenced})")
    for path, (status, detail) in sorted(accounting.items()):
        if not os.path.exists(os.path.join(args.pi, path)):
            problems.append(f"stale accounting row (pi test no longer exists): {path}")
            continue
        if status == "deferred":
            if not detail.strip():
                problems.append(f"deferred row has no reason: {path}")
            continue
        targets, _, reason = detail.partition("|")
        targets = [target.strip() for target in targets.split(",") if target.strip()]
        if not targets:
            problems.append(f"{status} row has no Rust test target: {path}")
        for target in targets:
            if not os.path.exists(os.path.join(ROOT, target)):
                problems.append(f"accounting target missing: {target} (for {path})")
        if status == "partial" and not reason.strip():
            problems.append(f"partial row has no reason: {path}")

    if problems:
        for problem in problems:
            print(f"check-pi-tests: {problem}", file=sys.stderr)
        counts = {"ported": 0, "partial": 0, "deferred": 0}
        for status, _ in accounting.values():
            counts[status] += 1
        print(
            f"check-pi-tests: {len(candidates)} referencing pi tests, "
            f"{counts['ported']} ported / {counts['partial']} partial / {counts['deferred']} deferred",
            file=sys.stderr,
        )
        return 1

    counts = {"ported": 0, "partial": 0, "deferred": 0}
    for status, _ in accounting.values():
        counts[status] += 1
    print(
        f"ok   {len(candidates)} pi tests referencing ported modules are accounted for "
        f"({counts['ported']} ported, {counts['partial']} partial, {counts['deferred']} deferred)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
