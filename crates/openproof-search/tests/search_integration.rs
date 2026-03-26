//! Integration tests for the tactic search pipeline.
//!
//! Requires a working Lean 4 toolchain with Mathlib and lean-lsp-mcp.
//! Run with:
//!
//!   cargo test -p openproof-search --test search_integration -- --ignored --nocapture

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use openproof_lean::lsp_mcp::LeanLspMcp;
use openproof_lean::tools::{find_sorry_positions, run_lean_verify_raw};
use openproof_search::config::{SearchResult, TacticSearchConfig};
use openproof_search::search::{best_first_search, ProposeFn};

fn lean_project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("lean")
}

fn standard_tactics() -> Vec<String> {
    vec![
        "simp", "omega", "ring", "norm_num", "linarith", "aesop",
        "grind", "decide", "trivial", "exact?", "apply?", "simp_all",
        "tauto", "contradiction", "norm_cast", "positivity", "gcongr",
        "polyrith", "field_simp", "push_cast", "ring_nf", "nlinarith",
        "norm_num [*]", "simp [*]", "grind?",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

fn make_propose_fn(tactics: Vec<String>) -> ProposeFn {
    Box::new(move |_goal: &str, _ctx: &str, k: usize| {
        Ok(tactics.iter().take(k).cloned().collect())
    })
}

/// Write a Lean file, spawn LSP, and warm it up by retrying get_diagnostics
/// until the Lean language server finishes its initial elaboration.
fn setup_lsp_test(content: &str) -> (Mutex<LeanLspMcp>, PathBuf, Vec<(usize, usize)>) {
    let project_dir = lean_project_dir();
    let scratch_path = project_dir.join("Scratch.lean");
    std::fs::write(&scratch_path, content).expect("write scratch file");

    let sorrys = find_sorry_positions(content);
    assert!(!sorrys.is_empty(), "Test content must have at least one sorry");

    // Spawn LSP and warm up with retries (first elaboration takes 30-90s)
    println!("Spawning lean-lsp-mcp and warming up (first load may take 60-90s)...");
    let warm_start = Instant::now();

    // Spawn, try get_diagnostics, if it fails (timeout), respawn and retry
    let max_retries = 4;
    let mut lsp = None;
    for attempt in 1..=max_retries {
        let client = LeanLspMcp::spawn(&project_dir)
            .expect("Failed to spawn lean-lsp-mcp");
        let mut client = client;

        // Try a diagnostics call to trigger elaboration
        match client.get_diagnostics(&scratch_path) {
            Ok(diag) => {
                println!(
                    "LSP warm (attempt {}) in {:.1}s, {} diagnostics",
                    attempt,
                    warm_start.elapsed().as_secs_f64(),
                    diag.items.len()
                );
                lsp = Some(client);
                break;
            }
            Err(e) => {
                println!(
                    "LSP warmup attempt {} failed after {:.1}s: {}",
                    attempt,
                    warm_start.elapsed().as_secs_f64(),
                    e
                );
                // Kill and retry
                client.close();
                if attempt == max_retries {
                    panic!("Failed to warm up LSP after {} attempts", max_retries);
                }
                // Brief pause before retry
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    }

    (Mutex::new(lsp.unwrap()), scratch_path, sorrys)
}

fn default_config() -> TacticSearchConfig {
    TacticSearchConfig {
        timeout: Duration::from_secs(120),
        ..TacticSearchConfig::default()
    }
}

fn print_result(result: &SearchResult) {
    match result {
        SearchResult::Solved { tactics, .. } => {
            println!("  SOLVED with {} tactics: {:?}", tactics.len(), tactics);
        }
        SearchResult::Partial { tactics, remaining_goals, .. } => {
            println!("  PARTIAL: {} goals remain, tactics: {:?}", remaining_goals, tactics);
        }
        SearchResult::Exhausted { expansions } => {
            println!("  EXHAUSTED after {} expansions", expansions);
        }
        SearchResult::Timeout { best_tactics, remaining_goals } => {
            println!("  TIMEOUT: {} goals remain, best: {:?}", remaining_goals, best_tactics);
        }
    }
}

// -----------------------------------------------------------------------
// Basic search tests (LSP-based)
// -----------------------------------------------------------------------

#[test]
#[ignore]
fn lsp_search_solves_nat_add_comm() {
    let content = "\
import Mathlib

theorem test_add_comm (a b : Nat) : a + b = b + a := by
  sorry
";
    let (lsp, scratch_path, sorrys) = setup_lsp_test(content);
    let propose_fn = make_propose_fn(standard_tactics());
    let config = default_config();

    let (line, _) = sorrys[0];
    println!("Searching sorry at line {} ...", line);
    let start = Instant::now();
    let result = best_first_search(&lsp, &propose_fn, &scratch_path, line, "", &config)
        .expect("search failed");
    println!("Completed in {:.2}s", start.elapsed().as_secs_f64());
    print_result(&result);

    assert!(result.is_solved(), "Expected Solved for nat add_comm");
}

#[test]
#[ignore]
fn lsp_search_solves_ring_identity() {
    let content = "\
import Mathlib

theorem test_ring (x : Int) : (x + 1) * (x + 1) = x * x + 2 * x + 1 := by
  sorry
";
    let (lsp, scratch_path, sorrys) = setup_lsp_test(content);
    let propose_fn = make_propose_fn(standard_tactics());
    let config = default_config();

    let (line, _) = sorrys[0];
    println!("Searching sorry at line {} ...", line);
    let start = Instant::now();
    let result = best_first_search(&lsp, &propose_fn, &scratch_path, line, "", &config)
        .expect("search failed");
    println!("Completed in {:.2}s", start.elapsed().as_secs_f64());
    print_result(&result);

    assert!(result.is_solved(), "Expected Solved for ring identity");
}

#[test]
#[ignore]
fn lsp_search_solves_omega_goal() {
    let content = "\
import Mathlib

theorem test_omega (n : Nat) : n < n + 1 := by
  sorry
";
    let (lsp, scratch_path, sorrys) = setup_lsp_test(content);
    let propose_fn = make_propose_fn(standard_tactics());
    let config = default_config();

    let (line, _) = sorrys[0];
    println!("Searching sorry at line {} ...", line);
    let start = Instant::now();
    let result = best_first_search(&lsp, &propose_fn, &scratch_path, line, "", &config)
        .expect("search failed");
    println!("Completed in {:.2}s", start.elapsed().as_secs_f64());
    print_result(&result);

    assert!(result.is_solved(), "Expected Solved for omega goal");
}

// -----------------------------------------------------------------------
// Test that grind tactic works
// -----------------------------------------------------------------------

#[test]
#[ignore]
fn lsp_search_grind_solves_equality_chain() {
    let content = "\
import Mathlib

theorem test_grind (a b c : Nat) (h1 : a = b) (h2 : b = c) : a = c := by
  sorry
";
    let (lsp, scratch_path, sorrys) = setup_lsp_test(content);

    // Only offer grind to prove it works
    let propose_fn: ProposeFn = Box::new(|_goal: &str, _ctx: &str, k: usize| {
        Ok(vec!["grind".to_string()].into_iter().take(k).collect())
    });
    let config = default_config();

    let (line, _) = sorrys[0];
    println!("Searching sorry at line {} with grind only ...", line);
    let start = Instant::now();
    let result = best_first_search(&lsp, &propose_fn, &scratch_path, line, "", &config)
        .expect("search failed");
    println!("Completed in {:.2}s", start.elapsed().as_secs_f64());
    print_result(&result);

    assert!(result.is_solved(), "Expected grind to solve equality chain");
}

// -----------------------------------------------------------------------
// End-to-end: search + verify
// -----------------------------------------------------------------------

#[test]
#[ignore]
fn end_to_end_search_then_verify() {
    let project_dir = lean_project_dir();
    let content_with_sorry = "\
import Mathlib

theorem e2e_test (n : Nat) : n + 0 = n := by
  sorry
";
    let (lsp, scratch_path, sorrys) = setup_lsp_test(content_with_sorry);
    let propose_fn = make_propose_fn(standard_tactics());
    let config = default_config();

    // Search for tactics to fill the sorry
    let (line, _) = sorrys[0];
    println!("[e2e] Searching sorry at line {}...", line);
    let start = Instant::now();
    let result = best_first_search(&lsp, &propose_fn, &scratch_path, line, "", &config)
        .expect("search failed");
    println!("[e2e] Search completed in {:.2}s", start.elapsed().as_secs_f64());
    print_result(&result);

    let tactics = match &result {
        SearchResult::Solved { tactics, .. } => tactics.clone(),
        other => panic!("[e2e] Expected Solved, got: {:?}", other),
    };

    // Step 3: Build filled proof and verify with lean compiler
    let tactic_block = tactics.join("\n  ");
    let content_filled = format!(
        "import Mathlib\n\ntheorem e2e_test (n : Nat) : n + 0 = n := by\n  {tactic_block}\n"
    );
    println!("[e2e] Verifying filled proof:\n{}", content_filled);

    let (ok, output) = run_lean_verify_raw(&project_dir, &content_filled)
        .expect("lean verify filled");
    if !output.is_empty() {
        println!("[e2e] Lean output: {}", &output[..output.len().min(500)]);
    }
    assert!(ok, "[e2e] Filled proof should verify. Output: {}", output);
    println!("[e2e] SUCCESS: search found tactics, lean verified the proof");
}

// -----------------------------------------------------------------------
// Multi-sorry in one file
// -----------------------------------------------------------------------

#[test]
#[ignore]
fn lsp_search_multi_sorry() {
    let content = "\
import Mathlib

theorem sorry1 (n : Nat) : 0 + n = n := by
  sorry

theorem sorry2 (a b : Nat) : a + b = b + a := by
  sorry

theorem sorry3 (n : Nat) : n * 1 = n := by
  sorry
";
    let (lsp, scratch_path, sorrys) = setup_lsp_test(content);
    let propose_fn = make_propose_fn(standard_tactics());
    let config = default_config();

    assert_eq!(sorrys.len(), 3, "Expected 3 sorrys");

    let mut solved = 0;
    for (i, &(line, _)) in sorrys.iter().enumerate() {
        println!("\n[multi] Sorry #{} at line {}...", i + 1, line);
        let start = Instant::now();
        let result = best_first_search(&lsp, &propose_fn, &scratch_path, line, "", &config)
            .expect("search failed");
        println!("[multi] Completed in {:.2}s", start.elapsed().as_secs_f64());
        print_result(&result);

        if result.is_solved() {
            solved += 1;
        }
    }

    println!("\n[multi] Results: {}/3 sorrys solved", solved);
    assert_eq!(solved, 3, "Expected all 3 sorrys to be solved");
}

// -----------------------------------------------------------------------
// Config: length penalty effect
// -----------------------------------------------------------------------

#[test]
#[ignore]
fn lsp_search_with_high_length_penalty() {
    let content = "\
import Mathlib

theorem test_penalty (n : Nat) : 0 + n = n := by
  sorry
";
    let (lsp, scratch_path, sorrys) = setup_lsp_test(content);
    let propose_fn = make_propose_fn(standard_tactics());

    let config = TacticSearchConfig {
        length_penalty: 1.0,
        timeout: Duration::from_secs(120),
        ..TacticSearchConfig::default()
    };

    let (line, _) = sorrys[0];
    println!("Searching with length_penalty=1.0 ...");
    let start = Instant::now();
    let result = best_first_search(&lsp, &propose_fn, &scratch_path, line, "", &config)
        .expect("search failed");
    println!("Completed in {:.2}s", start.elapsed().as_secs_f64());
    print_result(&result);

    if let SearchResult::Solved { tactics, .. } = &result {
        assert!(tactics.len() <= 2, "High penalty should produce short proof, got {} steps", tactics.len());
    }
    assert!(result.is_solved(), "Expected Solved even with high length penalty");
}
