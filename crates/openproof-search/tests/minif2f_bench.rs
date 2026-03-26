//! MiniF2F benchmark using pure tactic search (no LLM).
//!
//! Runs Pantograph best-first search on all 244 MiniF2F-test problems
//! using only standard automation tactics (simp, omega, ring, grind, etc.).
//!
//! Run:
//!   cargo test -p openproof-search --test minif2f_bench -- --ignored --nocapture 2>&1 | tee minif2f_results.txt

use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use openproof_lean::pantograph::Pantograph;
use openproof_search::config::{SearchResult, TacticSearchConfig};
use openproof_search::search::{pantograph_best_first_search, ProposeFn};

fn lean_project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("lean")
}

fn minif2f_test_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("benchmarks/miniF2F-lean4/MiniF2F/Test")
}

/// Extract the full type expression from a MiniF2F lean file.
///
/// Parses the theorem statement and converts it to a forall-expression
/// that Pantograph can use as a goal.
fn extract_type_expr(content: &str) -> Option<String> {
    // Find the theorem line(s) -- everything from "theorem" to ":= by sorry"
    let content = content.trim();

    // Remove imports, set_option, open lines
    let mut theorem_lines = Vec::new();
    let mut in_theorem = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("theorem ") || trimmed.starts_with("def ") {
            in_theorem = true;
        }
        if in_theorem {
            theorem_lines.push(line);
            if trimmed.contains(":= by sorry") || trimmed.contains(":= by") && trimmed.contains("sorry") {
                break;
            }
        }
    }

    if theorem_lines.is_empty() {
        return None;
    }

    let theorem_block = theorem_lines.join("\n");

    // Extract: everything between the first ":" after params and ":= by sorry"
    // Strategy: find the theorem signature, strip "theorem name", extract type

    // Remove "theorem <name>" prefix
    let after_theorem = if theorem_block.contains("theorem ") {
        let idx = theorem_block.find("theorem ").unwrap() + "theorem ".len();
        // Skip the name (first word after "theorem")
        let rest = &theorem_block[idx..];
        // The name ends at first space or newline or (
        let name_end = rest.find(|c: char| c.is_whitespace() || c == '(').unwrap_or(rest.len());
        rest[name_end..].trim()
    } else {
        return None;
    };

    // Now we have something like:
    //   (x y : Nat) (h : ...) : <goal> := by sorry
    // We need to extract the type, which means converting params to forall

    // Find the ":= by sorry" at the end
    let type_end = after_theorem.rfind(":= by")?;
    let signature = after_theorem[..type_end].trim();

    // The signature is: <params> : <conclusion>
    // We need to find the LAST top-level ":" that separates params from conclusion
    // This is tricky because params contain ":" too (e.g., "(x : Nat)")

    // Strategy: walk backwards from the end, tracking paren depth
    let sig_bytes = signature.as_bytes();
    let mut depth = 0i32;
    let mut colon_pos = None;

    for i in (0..sig_bytes.len()).rev() {
        match sig_bytes[i] {
            b')' | b']' | b'}' => depth += 1,
            b'(' | b'[' | b'{' => depth -= 1,
            b':' if depth == 0 => {
                // Check this isn't inside a word (like ":=")
                if i + 1 < sig_bytes.len() && sig_bytes[i + 1] == b'=' {
                    continue;
                }
                colon_pos = Some(i);
                break;
            }
            _ => {}
        }
    }

    let colon_pos = colon_pos?;
    let params_str = signature[..colon_pos].trim();
    let conclusion = signature[colon_pos + 1..].trim();

    if params_str.is_empty() {
        return Some(conclusion.to_string());
    }

    // Build forall expression from params
    // The params are like: (x y : Nat) (h : x > 0)
    // We wrap them: forall (x y : Nat) (h : x > 0), <conclusion>
    Some(format!("forall {params_str}, {conclusion}"))
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

#[test]
#[ignore]
fn minif2f_tactic_search_benchmark() {
    let project_dir = lean_project_dir();
    let test_dir = minif2f_test_dir();

    // Collect all problem files
    let mut problems: Vec<(String, String)> = Vec::new(); // (name, type_expr)
    let mut entries: Vec<_> = fs::read_dir(&test_dir)
        .expect("Failed to read miniF2F test dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map_or(false, |ext| ext == "lean"))
        .collect();
    entries.sort_by_key(|e| e.path());

    let mut parse_failures = 0;
    for entry in &entries {
        let path = entry.path();
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let content = fs::read_to_string(&path).unwrap();
        match extract_type_expr(&content) {
            Some(expr) => problems.push((name, expr)),
            None => {
                parse_failures += 1;
                eprintln!("PARSE_FAIL: {}", name);
            }
        }
    }

    println!("========================================");
    println!("MiniF2F Tactic Search Benchmark");
    println!("========================================");
    println!("Problems parsed: {}", problems.len());
    println!("Parse failures:  {}", parse_failures);
    println!("Lean version:    4.28.0");
    println!("Search config:   beam=8, expansions=200, timeout=120s, depth=20, penalty=0.1");
    println!("Tactics:         standard automation only (no LLM, no corpus)");
    println!("========================================\n");

    let config = TacticSearchConfig {
        beam_width: 8,
        max_expansions: 200,
        timeout: Duration::from_secs(120),
        dedup: true,
        max_depth: 20,
        length_penalty: 0.1,
    };

    let mut solved = 0usize;
    let mut failed = 0usize;
    let mut errored = 0usize;
    let mut total_time = Duration::ZERO;
    let mut solved_problems: Vec<(String, Vec<String>, f64)> = Vec::new();

    let total = problems.len();
    let bench_start = Instant::now();

    // Spawn Pantograph (respawn if it dies mid-benchmark)
    fn spawn_pg(project_dir: &std::path::Path) -> Mutex<Pantograph> {
        println!("Spawning Pantograph (Mathlib load)...");
        let start = Instant::now();
        let pg = Pantograph::spawn(project_dir).expect("Failed to spawn Pantograph");
        println!("Pantograph ready in {:.1}s", start.elapsed().as_secs_f64());
        Mutex::new(pg)
    }

    let mut pg = spawn_pg(&project_dir);

    for (i, (name, type_expr)) in problems.iter().enumerate() {
        // Check if Pantograph is still alive (or mutex poisoned), respawn if needed
        let needs_respawn = match pg.lock() {
            Ok(mut guard) => !guard.is_alive(),
            Err(_poisoned) => true,
        };
        if needs_respawn {
            eprintln!("  [respawning Pantograph]");
            pg = spawn_pg(&project_dir);
        }

        let propose_fn = make_propose_fn(standard_tactics());
        let start = Instant::now();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pantograph_best_first_search(&pg, &propose_fn, type_expr, "", &config)
        }));
        let elapsed = start.elapsed();
        total_time += elapsed;

        let status = match result {
            Ok(Ok(SearchResult::Solved { ref tactics, .. })) => {
                solved += 1;
                solved_problems.push((name.clone(), tactics.clone(), elapsed.as_secs_f64()));
                format!("SOLVED  {:.2}s  {:?}", elapsed.as_secs_f64(), tactics)
            }
            Ok(Ok(SearchResult::Partial { remaining_goals, .. })) => {
                failed += 1;
                format!("PARTIAL {:.2}s  {} goals remain", elapsed.as_secs_f64(), remaining_goals)
            }
            Ok(Ok(SearchResult::Exhausted { expansions })) => {
                failed += 1;
                format!("EXHAUST {:.2}s  {} expansions", elapsed.as_secs_f64(), expansions)
            }
            Ok(Ok(SearchResult::Timeout { remaining_goals, .. })) => {
                failed += 1;
                format!("TIMEOUT {:.2}s  {} goals remain", elapsed.as_secs_f64(), remaining_goals)
            }
            Ok(Err(e)) => {
                errored += 1;
                format!("ERROR   {:.2}s  {}", elapsed.as_secs_f64(), e)
            }
            Err(_panic) => {
                errored += 1;
                format!("PANIC   {:.2}s  search panicked", elapsed.as_secs_f64())
            }
        };

        println!("[{:3}/{}] {} -- {}", i + 1, total, status, name);
    }

    let wall_time = bench_start.elapsed();

    println!("\n========================================");
    println!("RESULTS");
    println!("========================================");
    println!("Total:    {}", total);
    println!("Solved:   {} ({:.1}%)", solved, 100.0 * solved as f64 / total as f64);
    println!("Failed:   {}", failed);
    println!("Errors:   {}", errored);
    println!("Wall time: {:.1}s", wall_time.as_secs_f64());
    println!("Avg time:  {:.2}s/problem", total_time.as_secs_f64() / total as f64);

    if !solved_problems.is_empty() {
        println!("\n--- Solved Problems ---");
        for (name, tactics, secs) in &solved_problems {
            println!("  {:.2}s  {}  {:?}", secs, name, tactics);
        }
    }

    println!("\npass@1 = {:.1}% ({}/{})", 100.0 * solved as f64 / total as f64, solved, total);
}
