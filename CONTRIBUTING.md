# Contributing to openproof

Thanks for your interest in contributing.

## Getting started

```bash
git clone https://github.com/markm39/openproof.git
cd openproof
cd lean && lake update && cd ..
cargo build --workspace
```

## Code guidelines

- Maximum 500 lines per file. Split by responsibility when approaching this limit.
- Follow Rust idioms: iterators over indexing, Result/Option over panicking.
- Run `cargo fmt` before committing.
- Run `cargo clippy -p <crate> -- -D warnings` and fix all warnings.

## Submitting changes

1. Create a branch: `git checkout -b feat/my-change`
2. Make your changes
3. Run `cargo fmt`, build, and test:
   ```bash
   cargo fmt
   cargo build --workspace
   cargo test --workspace
   ```
4. Commit with a conventional message: `feat(core): add new event type`
5. Push and open a pull request with a clear description
6. Ensure CI passes -- PRs require all checks green before merge

### Commit message format

Use [conventional commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <description>
```

Types: `feat`, `fix`, `refactor`, `chore`, `docs`, `test`

Scope is the crate name when relevant (e.g., `search`, `cli`, `protocol`).

## Reporting issues

Open an issue on GitHub with:
- What you expected to happen
- What actually happened
- Steps to reproduce
- Your environment (OS, Lean version, Rust version)
