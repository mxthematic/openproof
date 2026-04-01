//! Best-first tactic search over proof goals.
//!
//! Given a Lean file with sorry positions, this module systematically explores
//! candidate tactics at each sorry using the lean-lsp-mcp `screen_tactics` call.
//! Candidates are scored by remaining goal count and explored in priority order.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{bail, Result};

use openproof_lean::pantograph::Pantograph;

use openproof_protocol::{GoalStatus, ProofGoal};

use crate::cache::hash_goals;
use crate::config::{SearchResult, TacticSearchConfig};

/// Callback for emitting proof goal updates during search.
pub type GoalUpdateFn = dyn Fn(ProofGoal) + Send + Sync;

/// Callback for LLM tactic proposal. Returns candidate tactics for a goal state.
pub type ProposeFn = Box<dyn Fn(&str, &str, usize) -> Result<Vec<String>> + Send>;

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
        Self {
            priority,
            score,
            state_id,
            tactics,
            goal_descriptions,
        }
    }
}

impl PartialEq for PantographNode {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority
    }
}
impl Eq for PantographNode {}
impl PartialOrd for PantographNode {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for PantographNode {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        Reverse(self.priority)
            .cmp(&Reverse(other.priority))
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
    on_goal_update: Option<&GoalUpdateFn>,
) -> Result<SearchResult> {
    let start = Instant::now();
    let mut expansions: usize = 0;
    let mut seen_states: HashSet<u64> = HashSet::new();
    let mut frontier: BinaryHeap<PantographNode> = BinaryHeap::new();
    let mut allocated_states: Vec<u64> = Vec::new();

    // Start the proof goal
    let initial_state_id = {
        let mut pg = pantograph
            .lock()
            .map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        if !pg.is_alive() {
            bail!("Pantograph process is not running");
        }
        pg.start_goal(type_expr)?.ok_or_else(|| {
            anyhow::anyhow!(
                "goal.start failed for: {}",
                &type_expr[..type_expr.len().min(100)]
            )
        })?
    };
    allocated_states.push(initial_state_id);

    let initial_hash = {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&type_expr, &mut hasher);
        std::hash::Hasher::finish(&hasher)
    };
    seen_states.insert(initial_hash);

    let root_goal_id = format!("pg-{initial_state_id}");
    frontier.push(PantographNode::new(
        vec![type_expr.to_string()],
        vec![],
        initial_state_id,
        config.length_penalty,
    ));

    if let Some(cb) = on_goal_update {
        cb(ProofGoal {
            id: root_goal_id.clone(),
            goal_text: type_expr.to_string(),
            status: GoalStatus::Open,
            state_id: Some(initial_state_id),
            ..Default::default()
        });
    }

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
        let goal_text = node
            .goal_descriptions
            .first()
            .map(|s| s.as_str())
            .unwrap_or("");
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
                let mut pg = pantograph
                    .lock()
                    .map_err(|e| anyhow::anyhow!("lock: {e}"))?;
                if !pg.is_alive() {
                    cleanup_states(pantograph, &allocated_states);
                    bail!("Pantograph died during search");
                }
                pg.try_tactic(node.state_id, 0, tactic)?
            };
            expansions += 1;

            if result.error.is_some() || result.new_state_id.is_none() {
                eprintln!(
                    "  [bfs] tactic {tactic:?} FAILED: {:?}",
                    result.error.as_deref().unwrap_or("no state")
                );
                continue; // tactic failed
            }
            eprintln!(
                "  [bfs] tactic {tactic:?} OK: {} remaining goals, state={}",
                result.remaining_goals.len(),
                result.new_state_id.unwrap_or(0)
            );

            let new_state_id = result.new_state_id.unwrap();
            allocated_states.push(new_state_id);

            let child_goal_id = format!("pg-{new_state_id}");
            let parent_id = format!("pg-{}", node.state_id);

            // Proof complete!
            if result.remaining_goals.is_empty() {
                let mut tactics = node.tactics.clone();
                tactics.push(tactic.clone());
                if let Some(cb) = on_goal_update {
                    cb(ProofGoal {
                        id: child_goal_id,
                        goal_text: String::new(),
                        status: GoalStatus::Closed,
                        parent_goal_id: Some(parent_id),
                        tactic_applied: Some(tactic.clone()),
                        state_id: Some(new_state_id),
                        ..Default::default()
                    });
                }
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

            if let Some(cb) = on_goal_update {
                cb(ProofGoal {
                    id: child_goal_id,
                    goal_text: result.remaining_goals.join("\n"),
                    status: GoalStatus::Open,
                    parent_goal_id: Some(parent_id),
                    tactic_applied: Some(tactic.clone()),
                    state_id: Some(new_state_id),
                    ..Default::default()
                });
            }

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
