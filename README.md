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

OpenProof is an open-source conversational theorem prover. Describe a mathematical theorem in natural language, and OpenProof works with frontier LLMs to produce a machine-checked Lean 4 proof using Mathlib. Every proof candidate is verified by the Lean type checker -- if it passes, it's correct.

The autonomous proving loop combines **agentic reasoning** (LLM agents that write, patch, and repair whole Lean files) with **systematic tactic search** (a Rust-native best-first search engine that exhaustively explores candidate tactics at each sorry position). Both run in parallel -- first to solve a goal wins.

## Features

- **Conversational** -- describe theorems in plain language, iterate interactively
- **Machine-checked** -- every proof verified by the Lean 4 type checker
- **Autonomous mode** -- set a target and let it plan, prove, verify, and repair on its own
- **Hybrid search** -- agentic whole-file reasoning + systematic tactic search run in parallel
- **Structured goal access** -- integrates with [lean-lsp-mcp](https://github.com/oOo0oOo/lean-lsp-mcp) for structured proof goals and multi-tactic screening
- **Agent roles** -- specialized agents (planner, prover, repairer, retriever, critic) work on branches in parallel
- **Live dashboard** -- web dashboard with proof node graph and compiled PDF paper
- **Verified corpus** -- 190K+ searchable Mathlib declarations plus user-proven lemmas, retrieved automatically
- **Headless mode** -- `openproof run` for non-interactive autonomous proving, CI pipelines
- **Paper export** -- export proofs as LaTeX papers

## Quick start

### Prerequisites

- [Rust](https://rustup.rs/) (stable)
- [Lean 4](https://leanprover.github.io/lean4/doc/setup.html) with [Mathlib](https://github.com/leanprover-community/mathlib4)

**Optional** (enables structured goal states and fast multi-tactic screening):

```bash
uv tool install lean-lsp-mcp --python 3.12
```

**Optional** (local prover model for dramatically stronger tactic search):

```bash
brew install ollama && ollama serve
ollama pull hf.co/zeyu-zheng/BFS-Prover-V2-7B-GGUF:Q8_0
```

### Install

**npm** (easiest, no Rust toolchain needed):

```bash
npm install -g openproof
```

**Homebrew** (macOS / Linux):

```bash
brew tap markm39/tap && brew install openproof
```

**Shell installer**:

```bash
curl -fsSL https://raw.githubusercontent.com/markm39/openproof/master/scripts/install.sh | sh
```

**Cargo** (from source):

```bash
cargo install --path crates/openproof-cli
```

**Build from source**:

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

## CLI commands

| Command | Description |
|---------|-------------|
| `openproof` | Launch interactive TUI |
| `openproof run "<problem>" [--label <name>] [--resume <id>]` | Headless autonomous proving |
| `openproof health` | Check Lean toolchain, auth, and corpus status |
| `openproof ask "<prompt>"` | One-shot LLM query (no session) |
| `openproof dashboard [--open] [--port <port>]` | Start web dashboard server |
| `openproof login` | Sync authentication credentials |
| `openproof ingest` | Import Mathlib declarations into corpus |
| `openproof recluster-corpus` | Rebuild corpus vector embeddings |

## Slash commands

Inside the interactive TUI, type `/` followed by a command.

### Session management

| Command | Description |
|---------|-------------|
| `/new <title>` | Start a new session |
| `/resume <session-id>` | Switch to a different session (no args opens picker) |

### Proving

| Command | Description |
|---------|-------------|
| `/theorem <label> :: <statement>` | Create a theorem node |
| `/lemma <label> :: <statement>` | Create a lemma node |
| `/verify` | Trigger Lean verification manually |
| `/proof` | Show proof state and node tree |
| `/lean` | Inspect Lean state (scratch file, verification, history) |
| `/focus <id\|clear>` | Focus on a specific branch or node |
| `/autonomous start\|full\|stop\|step\|status` | Control autonomous proving loop |
| `/agent spawn <role> <task>` | Spawn a specific agent (planner, prover, repairer, retriever, critic) |

### Corpus and sync

| Command | Description |
|---------|-------------|
| `/corpus status` | Corpus statistics |
| `/corpus search <query>` | Search verified lemmas |
| `/corpus ingest` | Import Mathlib declarations |
| `/corpus recluster` | Rebuild embeddings |
| `/share [local\|community\|private]` | Set corpus share mode |
| `/sync status\|enable\|disable\|drain` | Control cloud sync |

### Inspection

| Command | Description |
|---------|-------------|
| `/nodes` | List all proof nodes |
| `/answer <option\|text>` | Answer a pending question |
| `/memory` | Show workspace memory |
| `/remember <text>` | Save to workspace memory |
| `/remember global <text>` | Save to global memory |

### Export

| Command | Description |
|---------|-------------|
| `/paper` | Display compiled paper |
| `/export paper\|tex\|lean\|all` | Export proof in various formats |
| `/dashboard` | Open web dashboard |

## Search strategies

OpenProof supports three proof search strategies, controlled by the `search_strategy` field on each session:

- **Hybrid** (default) -- both agentic reasoning and tactic search run in parallel at each sorry position. First to solve wins.
- **Agentic** -- LLM agents write and patch whole Lean files. The original behavior.
- **TacticSearch** -- pure best-first tactic search at each sorry, no agentic branches.

The tactic search engine uses [lean-lsp-mcp](https://github.com/oOo0oOo/lean-lsp-mcp) to screen multiple tactics in a single LSP round-trip, avoiding the per-tactic recompilation cost that would otherwise make exhaustive search infeasible. When lean-lsp-mcp is not installed, tactic search falls back to sequential `lake env lean` calls, and the agent tools (`lean_goals`, `lean_screen_tactics`) fall back to regex-based goal extraction.

## Agent roles

The autonomous loop spawns specialized agents on hidden branches:

| Role | Purpose |
|------|---------|
| **Planner** | Decomposes goals into sub-lemmas, decides proof strategy |
| **Prover** | Fills sorry positions with working tactics and proof code |
| **Repairer** | Fixes failed proofs using diagnostics and grounding facts |
| **Retriever** | Searches corpus and Mathlib for relevant lemmas |
| **Critic** | Reviews proofs for gaps, hidden assumptions, failure modes |

Spawn manually with `/agent spawn <role> <task description>`.

## Corpus

OpenProof maintains a verified corpus -- a searchable database of proven Lean declarations. When working on a new proof, relevant lemmas are automatically retrieved from the corpus and included in the model's context.

**Cloud mode** (recommended): Your verified proofs contribute to a shared corpus. In return, you get access to all community-verified lemmas. The more people prove, the better it gets.

**Local mode**: Corpus stays on your machine. Mathlib declarations are auto-imported on setup. No network calls.

## Architecture

OpenProof is a Rust workspace with 11 crates:

```
openproof-protocol   (shared types, enums, serde structs)
  -> openproof-store    (SQLite persistence, Qdrant embeddings)
  -> openproof-core     (AppState, event handling, proof logic)
  -> openproof-model    (LLM API calls, tool definitions)
  -> openproof-lean     (Lean toolchain, lean-lsp-mcp client)
  -> openproof-search   (best-first tactic search engine)
  -> openproof-corpus   (corpus search, ingest)
  -> openproof-cloud    (remote corpus sync)
  -> openproof-dashboard (web dashboard server)
  -> openproof-tui      (ratatui terminal rendering)
  -> openproof-cli      (binary: TUI shell, headless runner, commands)
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

MIT -- see [LICENSE](LICENSE).
