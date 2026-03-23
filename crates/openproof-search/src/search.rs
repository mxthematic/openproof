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

use crate::cache::{hash_goals, TacticCache};
use crate::config::{SearchResult, TacticSearchConfig};

/// A node in the search tree.
#[derive(Debug, Clone)]
struct SearchNode {
    /// Priority score: lower is better (fewer remaining goals).
    score: usize,
    /// Tactics applied so far from the initial state.
    tactics: Vec<String>,
    /// Goal strings after applying these tactics.
    goals: Vec<String>,
    /// The line in the file where the sorry is.
    sorry_line: usize,
}

impl PartialEq for SearchNode {
    fn eq(&self, other: &Self) -> bool {
        self.score == other.score
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
        // Min-heap by score (fewer goals = higher priority), break ties by shorter proof
        Reverse(self.score)
            .cmp(&Reverse(other.score))
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
        return Ok(SearchResult::Solved {
            tactics: vec![],
            file_content: String::new(),
        });
    }

    let initial_hash = hash_goals(&initial_goals);
    seen_states.insert(initial_hash);

    frontier.push(SearchNode {
        score: initial_goals.len(),
        tactics: vec![],
        goals: initial_goals.clone(),
        sorry_line,
    });

    let mut best_partial = SearchNode {
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

            frontier.push(SearchNode {
                score: new_goals.len(),
                tactics: new_tactics,
                goals: new_goals.clone(),
                sorry_line: node.sorry_line,
            });
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
