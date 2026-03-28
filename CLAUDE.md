# OpenProof Project Guidelines

## Build & Test

- Build: `cargo build` (workspace root)
- Build single crate: `cargo build -p <crate>`
- Test all: `cargo test --workspace`
- Test single crate: `cargo test -p <crate>`

## Architecture

Crate dependency flow:

```
openproof-protocol (types, enums, serde structs)
  -> openproof-store (SQLite persistence)
  -> openproof-core (AppState, event handling, proof logic)
  -> openproof-model (LLM API calls)
  -> openproof-lean (Lean toolchain interaction, lean-lsp-mcp client)
  -> openproof-search (best-first tactic search engine)
  -> openproof-corpus (corpus search, ingest)
  -> openproof-cloud (remote corpus sync)
  -> openproof-dashboard (web dashboard server)
  -> openproof-tui (ratatui rendering, custom terminal)
  -> openproof-cli (binary: TUI shell, commands, setup wizard)
```

## Development Workflow

### Branching
- `master` is protected. All changes go through pull requests.
- Create feature branches: `<type>/<short-description>`
  - Types: `feat`, `fix`, `refactor`, `chore`, `docs`, `test`
  - Example: `feat/kimina-integration`, `fix/sorry-verify-bug`

### Commits
- Use conventional commits: `<type>(<scope>): <description>`
- Scope is the crate name when relevant: `feat(search): add 32B model option`
- Breaking changes: `feat(protocol)!: rename SearchResult variants`
- Keep commits small and focused. One logical change per commit.

### Pull Requests
- PR title must be a conventional commit message (it becomes the squash commit)
- Include a summary, test plan, and affected crates
- CI must pass before merge
- Squash merge to master

### Releases
- Semantic versioning: bump version in workspace Cargo.toml
- Generate changelog: `git cliff -o CHANGELOG.md`
- Tag: `git tag v<version>` and push -- GitHub Actions handles the rest
- Update npm package versions via `scripts/publish-npm.sh` if publishing

## File Size Rule

- **Maximum 500 lines per file.** If a file approaches this limit, split it into focused modules.
- When splitting: extract by responsibility (e.g., `slash_commands.rs`, `event_loop.rs`, `turn_handling.rs`), not by arbitrary line count.
- Each module should have a clear single purpose described in its `//!` doc comment.
- Prefer many small focused files over few large ones.

## Code Conventions

- Follow Rust idioms: iterators over indexing, Result/Option over panicking
- Handle errors explicitly; don't swallow exceptions silently
- Keep functions small and focused on a single task
- Use `anyhow::Context` for error context in CLI commands

## Language Rules (IMPORTANT)

- Never describe the cloud server's purpose as "learning from failures" or "building a knowledge graph"
- Use neutral language: "cloud sync", "faster proofs", "larger corpus"
- Never mention "data moat", "flywheel", "cross-session learning", or "failure memory" in code, comments, commits, or docs
- Cloud mode messaging: "faster, more accurate proofs" -- never explain WHY it's better
- Commit messages should be purely technical, not strategic
- User-facing text should never reveal that failed attempts are collected or reused

## Before Committing

- `cargo fmt` (fix formatting)
- `cargo build -p <affected-crate>`
- `cargo test -p <affected-crate>` if tests exist
- `cargo clippy -p <affected-crate> -- -D warnings`
- Fix all warnings before committing
- **If you added, removed, or renamed any commands, features, CLI flags, tools, crates, or dependencies, update `README.md` in the same commit.** The README must stay accurate. Check the CLI commands table, slash commands tables, features list, and architecture diagram.
