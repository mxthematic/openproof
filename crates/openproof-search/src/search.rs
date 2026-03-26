//! Best-first tactic search over proof goals.
//!
//! Given a Lean file with sorry positions, this module systematically explores
//! candidate tactics at each sorry using the lean-lsp-mcp `screen_tactics` call.
//! Candidates are scored by remaining goal count and explored in priority order.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};
use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{bail, Result};

use openproof_lean::goal_state::AttemptResult;
use openproof_lean::lsp_mcp::LeanLspMcp;
use openproof_lean::pantograph::Pantograph;

use crate::cache::{hash_goals, TacticCache};
use crate::config::{SearchResult, TacticSearchConfig};

/// A node in the search tree.
#[derive(Debug, Clone)]
struct SearchNode {
    /// Length-normalized priority (milliunits): lower is better.
    /// Computed as `(remaining_goals + length_penalty * depth) * 1000`.
    priority: u64,
    /// Raw remaining goal count (used for best-partial tracking).
    score: usize,
    /// Tactics applied so far from the initial state.
    tactics: Vec<String>,
    /// Goal strings after applying these tactics.
    goals: Vec<String>,
    /// The line in the file where the sorry is.
    sorry_line: usize,
}

impl SearchNode {
    fn new(goals: Vec<String>, tactics: Vec<String>, sorry_line: usize, length_penalty: f64) -> Self {
        let score = goals.len();
        let priority = ((score as f64 + length_penalty * tactics.len() as f64) * 1000.0) as u64;
        Self { priority, score, tactics, goals, sorry_line }
    }
}

impl PartialEq for SearchNode {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority
    }
}

impl Eq for SearchNode {}

impl PartialOrd for SearchNode {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SearchNode {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Min-heap by priority (lower = better), break ties by shorter proof
        Reverse(self.priority)
            .cmp(&Reverse(other.priority))
            .then_with(|| self.tactics.len().cmp(&other.tactics.len()))
    }
}

/// Callback for LLM tactic proposal. Returns candidate tactics for a goal state.
pub type ProposeFn = Box<dyn Fn(&str, &str, usize) -> Result<Vec<String>> + Send>;

/// Run best-first search on a single sorry position.
///
/// Arguments:
/// - `lsp`: the lean-lsp-mcp client (mutex-protected for shared access)
/// - `propose_fn`: callback that asks the LLM to propose k tactics for a goal
/// - `file_path`: absolute path to the Lean file being worked on
/// - `sorry_line`: 1-indexed line number of the sorry to fill
/// - `retrieval_context`: corpus hits to include in the proposal prompt
/// - `config`: search parameters
pub fn best_first_search(
    lsp: &Mutex<LeanLspMcp>,
    propose_fn: &ProposeFn,
    file_path: &Path,
    sorry_line: usize,
    retrieval_context: &str,
    config: &TacticSearchConfig,
) -> Result<SearchResult> {
    let start = Instant::now();
    let mut expansions: usize = 0;
    let mut cache = TacticCache::new();
    let mut seen_states: HashSet<u64> = HashSet::new();
    let mut frontier: BinaryHeap<SearchNode> = BinaryHeap::new();

    // Get initial goal state
    let initial_goals = {
        let mut mcp = lsp.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        let goal_state = mcp.get_goals(file_path, sorry_line, None)?;
        goal_state
            .goals_before
            .or(goal_state.goals)
            .unwrap_or_default()
    };

    if initial_goals.is_empty() {
        // No goals at this position -- either already solved or not a sorry.
        // Return Exhausted rather than Solved with empty tactics, since we
        // didn't actually find the tactic that closes the goal.
        return Ok(SearchResult::Exhausted { expansions: 0 });
    }

    let initial_hash = hash_goals(&initial_goals);
    seen_states.insert(initial_hash);

    frontier.push(SearchNode::new(
        initial_goals.clone(),
        vec![],
        sorry_line,
        config.length_penalty,
    ));

    let mut best_partial = SearchNode {
        priority: u64::MAX,
        score: usize::MAX,
        tactics: vec![],
        goals: initial_goals,
        sorry_line,
    };

    while let Some(node) = frontier.pop() {
        // Check timeout
        if start.elapsed() > config.timeout {
            return Ok(SearchResult::Timeout {
                best_tactics: best_partial.tactics,
                remaining_goals: best_partial.score,
            });
        }

        // Check expansion budget
        if expansions >= config.max_expansions {
            return Ok(SearchResult::Exhausted { expansions });
        }

        // Track best partial result
        if node.score < best_partial.score {
            best_partial = node.clone();
        }

        // Use the first goal as the focus for tactic generation
        let goal_text = node.goals.first().map(|s| s.as_str()).unwrap_or("");
        if goal_text.is_empty() {
            continue;
        }

        // Ask LLM for candidate tactics
        let candidates = match propose_fn(goal_text, retrieval_context, config.beam_width) {
            Ok(c) => c,
            Err(_) => continue,
        };

        if candidates.is_empty() {
            continue;
        }

        // Filter out candidates we've already cached for this exact goal
        let mut to_screen: Vec<String> = Vec::new();
        let mut cached_results: Vec<(String, AttemptResult)> = Vec::new();

        for tactic in &candidates {
            if let Some(cached) = cache.get(goal_text, tactic) {
                cached_results.push((tactic.clone(), cached.clone()));
            } else {
                to_screen.push(tactic.clone());
            }
        }

        // Screen uncached tactics via LSP
        let mut screen_results: Vec<AttemptResult> = Vec::new();
        if !to_screen.is_empty() {
            let result = {
                let mut mcp = lsp.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
                if !mcp.is_alive() {
                    bail!("lean-lsp-mcp process died during search");
                }
                mcp.screen_tactics(file_path, node.sorry_line, None, &to_screen)?
            };

            for item in result.items {
                cache.insert(goal_text, &item.snippet, item.clone());
                screen_results.push(item);
            }
            expansions += to_screen.len();
        }

        // Process all results (cached + freshly screened)
        let all_results: Vec<AttemptResult> = cached_results
            .into_iter()
            .map(|(_, r)| r)
            .chain(screen_results)
            .collect();

        for item in all_results {
            if !item.succeeded() {
                continue;
            }

            // Goal closed
            if item.is_solved() {
                let mut tactics = node.tactics.clone();
                tactics.push(item.snippet);
                return Ok(SearchResult::Solved {
                    tactics,
                    file_content: String::new(), // caller fills this
                });
            }

            // New sub-goals
            let new_goals = &item.goals;
            let goals_hash = hash_goals(new_goals);

            // Transposition table: skip if we've seen this state
            if config.dedup && !seen_states.insert(goals_hash) {
                continue;
            }

            let mut new_tactics = node.tactics.clone();
            new_tactics.push(item.snippet.clone());

            frontier.push(SearchNode::new(
                new_goals.clone(),
                new_tactics,
                node.sorry_line,
                config.length_penalty,
            ));
        }
    }

    // Frontier exhausted
    if best_partial.score < usize::MAX && !best_partial.tactics.is_empty() {
        Ok(SearchResult::Partial {
            tactics: best_partial.tactics,
            remaining_goals: best_partial.score,
            file_content: String::new(),
        })
    } else {
        Ok(SearchResult::Exhausted { expansions })
    }
}

// ---------------------------------------------------------------------------
// Pantograph-native search (state-based, ~1000x faster)
// ---------------------------------------------------------------------------

/// A node in the Pantograph proof search tree.
#[derive(Debug, Clone)]
struct PantographNode {
    /// Length-normalized priority (milliunits): lower is better.
    priority: u64,
    /// Raw remaining goal count (used for best-partial tracking).
    score: usize,
    /// Pantograph state reference (immutable snapshot of proof state).
    state_id: u64,
    /// Tactic sequence from root to this state.
    tactics: Vec<String>,
    /// Goal descriptions for LLM proposal and dedup.
    goal_descriptions: Vec<String>,
}

impl PantographNode {
    fn new(
        goal_descriptions: Vec<String>,
        tactics: Vec<String>,
        state_id: u64,
        length_penalty: f64,
    ) -> Self {
        let score = goal_descriptions.len();
        let priority = ((score as f64 + length_penalty * tactics.len() as f64) * 1000.0) as u64;
        Self { priority, score, state_id, tactics, goal_descriptions }
    }
}

impl PartialEq for PantographNode {
    fn eq(&self, other: &Self) -> bool { self.priority == other.priority }
}
impl Eq for PantographNode {}
impl PartialOrd for PantographNode {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> { Some(self.cmp(other)) }
}
impl Ord for PantographNode {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        Reverse(self.priority).cmp(&Reverse(other.priority))
            .then_with(|| self.tactics.len().cmp(&other.tactics.len()))
    }
}

/// Best-first search using Pantograph goal states (~3ms per tactic test).
///
/// Instead of recompiling files, this operates directly on Pantograph's
/// proof state tree. Each tactic application returns a new state_id with
/// updated goals, enabling real tree search over proof strategies.
pub fn pantograph_best_first_search(
    pantograph: &Mutex<Pantograph>,
    propose_fn: &ProposeFn,
    type_expr: &str,
    retrieval_context: &str,
    config: &TacticSearchConfig,
) -> Result<SearchResult> {
    let start = Instant::now();
    let mut expansions: usize = 0;
    let mut seen_states: HashSet<u64> = HashSet::new();
    let mut frontier: BinaryHeap<PantographNode> = BinaryHeap::new();
    let mut allocated_states: Vec<u64> = Vec::new();

    // Start the proof goal
    let initial_state_id = {
        let mut pg = pantograph.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        if !pg.is_alive() {
            bail!("Pantograph process is not running");
        }
        pg.start_goal(type_expr)?
            .ok_or_else(|| anyhow::anyhow!("goal.start failed for: {}", &type_expr[..type_expr.len().min(100)]))?
    };
    allocated_states.push(initial_state_id);

    let initial_hash = {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&type_expr, &mut hasher);
        std::hash::Hasher::finish(&hasher)
    };
    seen_states.insert(initial_hash);

    frontier.push(PantographNode::new(
        vec![type_expr.to_string()],
        vec![],
        initial_state_id,
        config.length_penalty,
    ));

    let mut best_partial = PantographNode {
        priority: u64::MAX,
        score: usize::MAX,
        state_id: initial_state_id,
        tactics: vec![],
        goal_descriptions: vec![type_expr.to_string()],
    };

    while let Some(node) = frontier.pop() {
        if start.elapsed() > config.timeout {
            cleanup_states(pantograph, &allocated_states);
            return Ok(SearchResult::Timeout {
                best_tactics: best_partial.tactics,
                remaining_goals: best_partial.score,
            });
        }
        if expansions >= config.max_expansions {
            cleanup_states(pantograph, &allocated_states);
            return Ok(SearchResult::Exhausted { expansions });
        }
        if node.tactics.len() >= config.max_depth {
            continue; // prune deep branches
        }
        if node.score < best_partial.score {
            best_partial = node.clone();
        }

        // Focus on the first goal description for tactic proposal
        let goal_text = node.goal_descriptions.first().map(|s| s.as_str()).unwrap_or("");
        if goal_text.is_empty() {
            continue;
        }

        // Generate candidate tactics
        let candidates = match propose_fn(goal_text, retrieval_context, config.beam_width) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Test each candidate tactic via Pantograph (3ms each)
        for tactic in &candidates {
            let result = {
                let mut pg = pantograph.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
                if !pg.is_alive() {
                    cleanup_states(pantograph, &allocated_states);
                    bail!("Pantograph died during search");
                }
                pg.try_tactic(node.state_id, 0, tactic)?
            };
            expansions += 1;

            if !result.error.is_none() || result.new_state_id.is_none() {
                continue; // tactic failed
            }

            let new_state_id = result.new_state_id.unwrap();
            allocated_states.push(new_state_id);

            // Proof complete!
            if result.remaining_goals.is_empty() {
                let mut tactics = node.tactics.clone();
                tactics.push(tactic.clone());
                cleanup_states(pantograph, &allocated_states);
                return Ok(SearchResult::Solved {
                    tactics,
                    file_content: String::new(),
                });
            }

            // New state with reduced goals -- add to frontier
            let goals_hash = hash_goals(&result.remaining_goals);
            if config.dedup && !seen_states.insert(goals_hash) {
                continue; // already explored this goal set
            }

            let mut new_tactics = node.tactics.clone();
            new_tactics.push(tactic.clone());

            frontier.push(PantographNode::new(
                result.remaining_goals,
                new_tactics,
                new_state_id,
                config.length_penalty,
            ));
        }
    }

    cleanup_states(pantograph, &allocated_states);

    if best_partial.score < usize::MAX && !best_partial.tactics.is_empty() {
        Ok(SearchResult::Partial {
            tactics: best_partial.tactics,
            remaining_goals: best_partial.score,
            file_content: String::new(),
        })
    } else {
        Ok(SearchResult::Exhausted { expansions })
    }
}

/// Delete all Pantograph goal states to free REPL memory.
fn cleanup_states(pantograph: &Mutex<Pantograph>, states: &[u64]) {
    if let Ok(mut pg) = pantograph.lock() {
        for &sid in states {
            let _ = pg.delete_goal(sid);
        }
    }
}
