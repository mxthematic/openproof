//! Integration tests for Pantograph fast paths and LSP verification.
//!
//! Requires Pantograph/lean-lsp-mcp and Lean with Mathlib.
//! Tests marked #[ignore] need external tooling.

use openproof_lean::lsp_mcp::LeanLspMcp;
use openproof_lean::proof_tree::SessionProver;
use openproof_lean::tools::{execute_tool, ToolContext};
use openproof_lean::verify_scratch_via_lsp;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

fn lean_project_dir() -> &'static Path {
    // cargo test runs from the package root (crates/openproof-lean/),
    // but the lean project is at the workspace root's lean/ directory.
    Path::new("../../lean")
}

fn spawn_prover() -> Option<Arc<Mutex<SessionProver>>> {
    match SessionProver::spawn(lean_project_dir()) {
        Ok(p) => Some(Arc::new(Mutex::new(p))),
        Err(e) => {
            eprintln!("SessionProver::spawn failed: {e:#}");
            None
        }
    }
}

#[test]
#[ignore] // Requires Pantograph + Mathlib (~18s startup)
fn pantograph_single_expr() {
    let prover = spawn_prover().expect("Pantograph not available");
    let ctx = ToolContext {
        project_dir: lean_project_dir(),
        workspace_dir: Path::new("/tmp"),
        imports: &[],
        lsp_mcp: None,
        prover: Some(prover),
    };

    let start = Instant::now();
    let result = execute_tool("lean_check", r#"{"expr": "deriv_add"}"#, &ctx);
    let elapsed = start.elapsed();

    assert!(result.success, "lean_check failed: {}", result.content);
    assert!(
        result.content.contains("deriv_add"),
        "output should contain expression name: {}",
        result.content
    );
    assert!(
        result.content.contains("deriv"),
        "output should contain type info: {}",
        result.content
    );
    // Pantograph path should be fast (< 2s, typically <100ms)
    assert!(
        elapsed.as_secs() < 2,
        "Pantograph inspect took too long: {elapsed:?}"
    );
}

#[test]
#[ignore] // Requires Pantograph + Mathlib (~18s startup)
fn pantograph_batch_exprs() {
    let prover = spawn_prover().expect("Pantograph not available");
    let ctx = ToolContext {
        project_dir: lean_project_dir(),
        workspace_dir: Path::new("/tmp"),
        imports: &[],
        lsp_mcp: None,
        prover: Some(prover),
    };

    let start = Instant::now();
    let result = execute_tool(
        "lean_check",
        r#"{"exprs": ["deriv_add", "Nat.Prime.dvd_mul", "List.map"]}"#,
        &ctx,
    );
    let elapsed = start.elapsed();

    assert!(
        result.success,
        "batch lean_check failed: {}",
        result.content
    );
    assert!(
        result.content.contains("deriv_add"),
        "should contain deriv_add"
    );
    assert!(
        result.content.contains("Nat.Prime.dvd_mul"),
        "should contain Nat.Prime.dvd_mul"
    );
    assert!(
        result.content.contains("List.map"),
        "should contain List.map"
    );
    // 3 expressions via Pantograph should still be fast
    assert!(
        elapsed.as_secs() < 3,
        "Batch Pantograph inspect took too long: {elapsed:?}"
    );
}

#[test]
#[ignore] // Requires Pantograph + Mathlib (~18s startup)
fn pantograph_unknown_expr() {
    let prover = spawn_prover().expect("Pantograph not available");
    let ctx = ToolContext {
        project_dir: lean_project_dir(),
        workspace_dir: Path::new("/tmp"),
        imports: &[],
        lsp_mcp: None,
        prover: Some(prover),
    };

    let result = execute_tool(
        "lean_check",
        r#"{"expr": "totally_fake_lemma_xyz_12345"}"#,
        &ctx,
    );
    // Should not crash; returns success=false with descriptive message
    assert!(!result.success);
    assert!(
        result.content.contains("not found") || result.content.contains("error"),
        "should indicate not found: {}",
        result.content
    );
}

// --- LSP verify tests ---

fn spawn_lsp() -> Option<Arc<Mutex<LeanLspMcp>>> {
    match LeanLspMcp::spawn(lean_project_dir()) {
        Ok(client) => Some(Arc::new(Mutex::new(client))),
        Err(e) => {
            eprintln!("LeanLspMcp::spawn failed: {e:#}");
            None
        }
    }
}

#[test]
#[ignore] // Requires lean-lsp-mcp + Mathlib
fn lsp_verify_valid_proof() {
    let lsp = spawn_lsp().expect("lean-lsp-mcp not available");

    let content = "import Mathlib\n\ntheorem foo : 1 + 1 = 2 := by norm_num\n".to_string();

    let start = Instant::now();
    let result = verify_scratch_via_lsp(&lsp, lean_project_dir(), content).unwrap();
    let elapsed = start.elapsed();

    eprintln!(
        "lsp_verify_valid_proof: ok={}, elapsed={elapsed:?}, stderr={}",
        result.ok, result.stderr
    );
    assert!(result.ok, "valid proof should verify: {}", result.stderr);
}

#[test]
#[ignore] // Requires lean-lsp-mcp + Mathlib
fn lsp_verify_sorry_detected() {
    let lsp = spawn_lsp().expect("lean-lsp-mcp not available");

    let content = "import Mathlib\n\ntheorem bar : 1 + 1 = 2 := by sorry\n".to_string();

    let result = verify_scratch_via_lsp(&lsp, lean_project_dir(), content).unwrap();

    eprintln!(
        "lsp_verify_sorry: ok={}, error={:?}, stderr={}",
        result.ok, result.error, result.stderr
    );
    assert!(!result.ok, "sorry proof should fail: {}", result.stderr);
    assert_eq!(
        result.error.as_deref(),
        Some("sorry-placeholder"),
        "should detect sorry"
    );
}

#[test]
#[ignore] // Requires lean-lsp-mcp + Mathlib
fn lsp_verify_type_error() {
    let lsp = spawn_lsp().expect("lean-lsp-mcp not available");

    let content = "import Mathlib\n\ntheorem baz : 1 + 1 = 3 := by norm_num\n".to_string();

    let result = verify_scratch_via_lsp(&lsp, lean_project_dir(), content).unwrap();

    eprintln!(
        "lsp_verify_type_error: ok={}, stderr={}",
        result.ok, result.stderr
    );
    assert!(!result.ok, "wrong proof should fail: {}", result.stderr);
}

#[test]
#[ignore] // Requires lean-lsp-mcp + Mathlib
fn lsp_verify_incremental_is_fast() {
    let lsp = spawn_lsp().expect("lean-lsp-mcp not available");

    // First call: cold start (may be slow)
    let content1 = "import Mathlib\n\ntheorem warmup : 1 = 1 := rfl\n".to_string();
    let _ = verify_scratch_via_lsp(&lsp, lean_project_dir(), content1);

    // Second call: should be incremental (fast)
    let content2 = "import Mathlib\n\ntheorem fast_check : 2 + 2 = 4 := by norm_num\n".to_string();
    let start = Instant::now();
    let result = verify_scratch_via_lsp(&lsp, lean_project_dir(), content2).unwrap();
    let elapsed = start.elapsed();

    eprintln!(
        "lsp_verify_incremental: ok={}, elapsed={elapsed:?}",
        result.ok
    );
    assert!(result.ok, "incremental verify failed: {}", result.stderr);
    // Incremental should be much faster than cold (~2s vs ~18s)
    assert!(
        elapsed.as_secs() < 30,
        "incremental verify too slow: {elapsed:?} (expected <30s)"
    );
}

#[test]
fn lean_check_missing_args() {
    let ctx = ToolContext {
        project_dir: lean_project_dir(),
        workspace_dir: Path::new("/tmp"),
        imports: &[],
        lsp_mcp: None,
        prover: None,
    };

    let result = execute_tool("lean_check", r#"{}"#, &ctx);
    assert!(!result.success);
    assert!(
        result.content.contains("missing"),
        "should report missing args: {}",
        result.content
    );
}
