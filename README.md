# xjpcode

A coding agent for the terminal, written in Rust.

You give it a task in plain language. It runs a model in a loop and lets the model read files, run shell commands, and edit and write files until the task is done.

Below the interface, xjpcode is a port to Rust of [pi](https://github.com/earendil-works/pi) 1.6.0. The agent loop, the model interface, the four tools, session files, compaction and retry follow pi's behaviour, and pi's tests were ported together with the code. The full-screen interface is xjpcode's own.

## Status

Work in progress. This repository is a snapshot of the source, published without its earlier history.

Comments in the source cite `PORTING.md` and files under `docs/port/`. Those are internal working notes and are not part of this repository.

## Build and run

Build from a Git checkout. The build records the commit id, so a source archive without `.git` will not build as it is.

```sh
git clone https://github.com/RuoqiJin/xjpcode.git
cd xjpcode
cargo build --release
```

xjpcode needs a DeepSeek API key. Put it in the environment, or start the interface and type `/login`.

```sh
export DEEPSEEK_API_KEY=...
```

```sh
cargo run --release -p xjpcode                             # the full-screen interface
cargo run --release -p xjpcode -- "fix the failing test"   # one prompt, plain output, then exit
cargo run --release -p xjpcode -- transcript               # a plain line-by-line interface
cargo run --release -p xjpcode -- resume                   # continue the latest session in this directory
```

Credentials and sessions are kept under `~/.xjpcode/agent/`.

## Layout

| Path | What it is |
|---|---|
| `crates/ai` | The model interface: providers, streaming, authentication |
| `crates/agent` | The agent loop |
| `crates/coding-agent` | The four tools, the system prompt, rules and skills, session files, compaction, retry |
| `crates/schema` | Schema validation, used for tool arguments |
| `crates/cli` | `xjpcode-new`, a plain command-line entry to the same core |
| `src/` | The terminal interface and the `xjpcode` binary |
| `scripts/port/` | Scripts and data that compare this port with pi |

## Tests

```sh
cargo test --workspace
```

## Credits

The core is ported from [pi](https://github.com/earendil-works/pi) by Mario Zechner, which is MIT licensed. [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) lists what comes from pi and reproduces its licence.

## License

MIT. See [LICENSE](LICENSE).

---

# xjpcode(中文)

一个在终端里用的编程助手,用 Rust 写的。

你用大白话交给它一件事。它让模型一轮一轮地干:读文件、跑命令、改文件、写文件,直到做完。

界面以下的部分,是把 [pi](https://github.com/earendil-works/pi) 1.6.0 搬成 Rust:内核循环、模型接口、四样工具、会话文件、压缩和重试都照 pi 的行为,pi 的测试也跟着代码一起搬了过来。全屏界面是 xjpcode 自己的。

## 现状

还在做。这个仓库是源码的一份快照,公开时没有带以前的修改历史。

源码注释里提到的 `PORTING.md` 和 `docs/port/` 下的文件是内部的工作记录,没有放进这个仓库。

## 构建和运行

要从 Git 检出的目录里构建。构建时会记下提交号,所以不带 `.git` 的源码包直接构建不了。

```sh
git clone https://github.com/RuoqiJin/xjpcode.git
cd xjpcode
cargo build --release
```

需要一把 DeepSeek 的 API 钥匙:放进环境变量,或者进界面以后输入 `/login`。

```sh
export DEEPSEEK_API_KEY=...
```

```sh
cargo run --release -p xjpcode                        # 全屏界面
cargo run --release -p xjpcode -- "把挂掉的测试修好"   # 只说一句,朴素输出,做完退出
cargo run --release -p xjpcode -- transcript          # 朴素的一行一行的界面
cargo run --release -p xjpcode -- resume              # 接着这个目录里最近的一场聊
```

钥匙和会话都存在 `~/.xjpcode/agent/` 下。

## 目录

| 路径 | 是什么 |
|---|---|
| `crates/ai` | 模型接口:各家模型、流式、登录 |
| `crates/agent` | 内核循环 |
| `crates/coding-agent` | 四样工具、系统提示词、规矩与技能、会话文件、压缩、重试 |
| `crates/schema` | 结构校验,用来校验工具的参数 |
| `crates/cli` | `xjpcode-new`:同一个内核的朴素命令行入口 |
| `src/` | 终端界面和 `xjpcode` 这个程序 |
| `scripts/port/` | 拿这份移植和 pi 做对照的脚本与数据 |

## 测试

```sh
cargo test --workspace
```

## 致谢

内核移植自 Mario Zechner 的 [pi](https://github.com/earendil-works/pi)(MIT 许可)。哪些东西来自 pi、它的许可原文,见 [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)。

## 许可

MIT,见 [LICENSE](LICENSE)。
