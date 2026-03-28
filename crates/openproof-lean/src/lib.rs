//! Lean toolchain interaction: verification, parsing, rendering, goal extraction.

pub mod corpus_module;
pub mod goal_state;
pub mod goals;
pub mod lsp_mcp;
pub mod pantograph;
pub mod parse;
pub mod patch;
pub mod proof_tree;
pub mod render;
pub mod tools;
pub mod verify;

// Re-export primary public API for backward compatibility.
pub use goals::{extract_grounding_from_lean_output, extract_sorry_goals, run_tactic_suggestions};
pub use parse::{declarations_to_proof_nodes, parse_lean_declarations, LeanDeclaration};
pub use render::render_node_scratch;
pub use tools::find_sorry_positions;
pub use verify::{
    detect_lean_health, verify_active_node, verify_node, verify_node_at, verify_scratch_content,
    verify_scratch_via_lsp,
};
