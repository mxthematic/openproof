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
- Run `cargo build -p <crate>` and fix warnings before committing.
- Keep commits focused. One logical change per commit.

## Submitting changes

1. Fork the repo and create a branch
2. Make your changes
3. Ensure it builds: `cargo build --workspace`
4. Open a pull request with a clear description of the change

## Reporting issues

Open an issue on GitHub with:
- What you expected to happen
- What actually happened
- Steps to reproduce
- Your environment (OS, Lean version, Rust version)
