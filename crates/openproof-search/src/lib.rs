//! Best-first tactic search engine for Lean proofs.
//!
//! Explores candidate tactics at sorry positions using structured goal states
//! from lean-lsp-mcp, scoring by remaining goal count and deduplicating via
//! a transposition table.

pub mod cache;
pub mod config;
pub mod ollama;
pub mod search;
