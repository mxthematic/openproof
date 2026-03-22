# openproof

```
  ___                   ____                    __
 / _ \ _ __   ___ _ __ |  _ \ _ __ ___   ___  / _|
| | | | '_ \ / _ \ '_ \| |_) | '__/ _ \ / _ \| |_
| |_| | |_) |  __/ | | |  __/| | | (_) | (_) |  _|
 \___/| .__/ \___|_| |_|_|   |_|  \___/ \___/|_|
      |_|
```

Formal math proofs, conversationally.

## What is this

openproof is an open-source conversational theorem prover. Describe a mathematical theorem in natural language, and openproof works with frontier LLMs to produce a machine-checked Lean 4 proof using Mathlib. Every proof candidate is verified by the Lean type checker -- if it passes, it's correct.

## Features

- **Conversational** -- describe theorems in plain language, iterate interactively
- **Machine-checked** -- every proof verified by the Lean 4 type checker, no hallucinated results
- **Autonomous mode** -- set a target and let it plan, prove, verify, and repair on its own
- **Live dashboard** -- web dashboard with proof node graph and compiled PDF paper
- **Verified corpus** -- growing searchable library of proven lemmas, retrieved automatically for future proofs
- **Bring your own compute** -- works with your existing model subscriptions (ChatGPT, OpenAI API, Anthropic)
- **Local-first** -- sessions, proofs, and corpus stored locally in SQLite

## Quick start

### Prerequisites

- [Rust](https://rustup.rs/) (stable)
- [Lean 4](https://leanprover.github.io/lean4/doc/setup.html) with [Mathlib](https://github.com/leanprover-community/mathlib4)

### Install

```bash
cargo install --path crates/openproof-cli
```

Or build from source:

```bash
git clone https://github.com/markm39/openproof.git
cd openproof
cd lean && lake update && cd ..
cargo build --release
```

### Run

```bash
openproof
```

On first launch, a setup wizard guides you through model provider selection and corpus mode.

## Usage

Type a math problem or theorem statement to start. openproof parses it, formalizes a Lean target, and iterates toward a verified proof.

### Commands

| Command | Description |
|---------|-------------|
| `/help` | Show all commands |
| `/status` | Current session status |
| `/proof` | Show proof state |
| `/verify` | Manually trigger Lean verification |
| `/dashboard` | Open web dashboard in browser |
| `/autonomous start` | Start autonomous proving loop |
| `/autonomous stop` | Stop autonomous loop |
| `/new <title>` | Start a new session |
| `/resume` | Switch session (interactive picker) |
| `/sessions` | List all sessions |
| `/corpus ingest` | Import Mathlib declarations into corpus |
| `/export paper` | Export proof as LaTeX paper |

### Keyboard shortcuts

| Key | Action |
|-----|--------|
| Up/Down | Browse input history |
| Esc | Abort current turn / clear input |
| Ctrl+C | Clear input (first), quit (second) |
| `/` | Enter command mode (with tab completion) |
| PageUp/PageDown | Scroll through conversation |

## Corpus

openproof maintains a verified corpus -- a searchable database of proven Lean declarations. When working on a new proof, relevant lemmas are automatically retrieved from the corpus and included in the model's context.

**Cloud mode** (recommended): Your verified proofs contribute to a shared corpus. In return, you get access to all community-verified lemmas. The more people prove, the better it gets.

**Local mode**: Corpus stays on your machine. Mathlib declarations are auto-imported on setup. No network calls.

## Architecture

| Crate | Purpose |
|-------|---------|
| `openproof-cli` | Binary: TUI shell, commands, setup wizard |
| `openproof-tui` | Terminal rendering (ratatui) with inline viewport |
| `openproof-core` | Application state machine and event handling |
| `openproof-store` | SQLite persistence (sessions, corpus, sync) |
| `openproof-protocol` | Shared types (serde-serializable) |
| `openproof-model` | LLM API integration |
| `openproof-lean` | Lean toolchain interaction and verification |
| `openproof-dashboard` | Web dashboard server (Axum) |
| `openproof-cloud` | Remote corpus API client |
| `openproof-corpus` | Corpus orchestration (ingestion, search) |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

MIT -- see [LICENSE](LICENSE).
