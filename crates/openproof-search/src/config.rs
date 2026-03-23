//! Configuration and result types for tactic search.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Configuration for a tactic search run.
#[derive(Debug, Clone)]
pub struct TacticSearchConfig {
    /// Number of candidate tactics to request from the LLM per goal state.
    pub beam_width: usize,
    /// Maximum total tactic applications before giving up.
    pub max_expansions: usize,
    /// Timeout for the entire search on one sorry.
    pub timeout: Duration,
    /// Whether to use the transposition table (dedup by goal hash).
    pub dedup: bool,
}

impl Default for TacticSearchConfig {
    fn default() -> Self {
        Self {
            beam_width: 8,
            max_expansions: 200,
            timeout: Duration::from_secs(120),
            dedup: true,
        }
    }
}

/// Result of a tactic search.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum SearchResult {
    /// Tactic sequence closes the goal completely.
    Solved {
        tactics: Vec<String>,
        file_content: String,
    },
    /// Made progress but didn't close -- fewer goals remain.
    Partial {
        tactics: Vec<String>,
        remaining_goals: usize,
        file_content: String,
    },
    /// No progress possible -- all candidates exhausted.
    Exhausted {
        expansions: usize,
    },
    /// Time limit hit.
    Timeout {
        best_tactics: Vec<String>,
        remaining_goals: usize,
    },
}

impl SearchResult {
    pub fn is_solved(&self) -> bool {
        matches!(self, SearchResult::Solved { .. })
    }
}
