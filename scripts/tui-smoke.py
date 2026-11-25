#!/usr/bin/env python3
"""PTY smoke test for xjpcode TUI terminal lifecycle.

This intentionally runs the compiled binary in a pseudo-terminal so crossterm
uses the same terminal control path as interactive use.
"""

from __future__ import annotations

import os
import pty
import select
import subprocess
import sys
import time
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
BINARY = ROOT / "target" / "debug" / "xjpcode"


def main() -> int:
    if not BINARY.exists():
        print(f"missing binary: {BINARY}; run `cargo build` first", file=sys.stderr)
        return 2

    master, slave = pty.openpty()
    env = os.environ.copy()
    env.setdefault("TERM", "xterm-256color")
    env["XJPCODE_TUI_DEBUG"] = "1"

    proc = subprocess.Popen(
        [str(BINARY), "tui", "smoke"],
        stdin=slave,
        stdout=slave,
        stderr=slave,
        cwd=str(ROOT),
        env=env,
        close_fds=True,
    )
    os.close(slave)

    chunks: list[bytes] = []
    def output_has_success() -> bool:
        output = b"".join(chunks)
        return (
            b"xjpcode tui smoke ok" in output
            and b"\x1b[?1049h" in output
            and b"\x1b[?1049l" in output
        )

    deadline = time.monotonic() + 10.0
    while time.monotonic() < deadline:
        ready, _, _ = select.select([master], [], [], 0.1)
        if master in ready:
            try:
                data = os.read(master, 8192)
            except OSError:
                break
            if not data:
                break
            chunks.append(data)
            if output_has_success():
                break
        if proc.poll() is not None:
            break

    if proc.poll() is None:
        if output_has_success():
            proc.terminate()
            try:
                proc.wait(timeout=1)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=2)
        else:
            proc.kill()
            proc.wait(timeout=2)
            output = b"".join(chunks).decode("utf-8", errors="replace")
            if output:
                print(output)
            print("xjpcode tui smoke timed out", file=sys.stderr)
            return 1

    try:
        while True:
            ready, _, _ = select.select([master], [], [], 0)
            if master not in ready:
                break
            data = os.read(master, 8192)
            if not data:
                break
            chunks.append(data)
    except OSError:
        pass
    finally:
        os.close(master)

    output = b"".join(chunks)
    decoded = output.decode("utf-8", errors="replace")
    if "xjpcode tui smoke ok" not in decoded:
        print(decoded)
        print("xjpcode tui smoke did not report success", file=sys.stderr)
        return 1
    if b"\x1b[?1049h" not in output or b"\x1b[?1049l" not in output:
        print(decoded)
        print("alternate screen enter/leave sequences were not observed", file=sys.stderr)
        return 1
    mouse_capture_sequences = [
        b"\x1b[?1000h",
        b"\x1b[?1002h",
        b"\x1b[?1003h",
        b"\x1b[?1006h",
    ]
    if not any(seq in output for seq in mouse_capture_sequences):
        print(decoded)
        print("default TUI smoke did not enable mouse capture for trackpad scrolling", file=sys.stderr)
        return 1
    if proc.returncode not in (0, None, -15):
        print(decoded)
        print(f"xjpcode tui smoke exited {proc.returncode}", file=sys.stderr)
        return 1
    print("xjpcode tui PTY smoke passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
