//! Shared proof tree state for coordinating agent + tactic search.
//!
//! Tracks failed tactics per goal so neither the agent nor the search
//! retries tactics that are known to fail. Wraps a Pantograph instance
//! so one REPL process serves the entire session.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Result;

use crate::pantograph::Pantograph;

/// Tracks which tactics have failed for each goal state.
/// Keyed by a hash of the goal description text.
#[derive(Debug, Default)]
pub struct ProofTreeState {
    failed_tactics: HashMap<u64, HashSet<String>>,
    /// Total number of tactic attempts across all goals.
    pub total_attempts: usize,
    /// Total number of failures.
    pub total_failures: usize,
}

impl ProofTreeState {
    /// Record that a tactic failed for a given goal.
    pub fn record_failure(&mut self, goal_hash: u64, tactic: &str) {
        self.failed_tactics
            .entry(goal_hash)
            .or_default()
            .insert(tactic.to_string());
        self.total_failures += 1;
    }

    /// Record a tactic attempt (success or failure).
    pub fn record_attempt(&mut self) {
        self.total_attempts += 1;
    }

    /// Check if a tactic is known to fail for a given goal.
    pub fn is_known_failure(&self, goal_hash: u64, tactic: &str) -> bool {
        self.failed_tactics
            .get(&goal_hash)
            .map(|set| set.contains(tactic))
            .unwrap_or(false)
    }

    /// Get all failed tactics for a goal.
    pub fn failures_for_goal(&self, goal_hash: u64) -> Vec<String> {
        self.failed_tactics
            .get(&goal_hash)
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Number of distinct goals that have been explored.
    pub fn explored_goals(&self) -> usize {
        self.failed_tactics.len()
    }

    /// Clear all state (e.g., on session change).
    pub fn clear(&mut self) {
        self.failed_tactics.clear();
        self.total_attempts = 0;
        self.total_failures = 0;
    }
}

/// Hash a goal description string for use as a ProofTreeState key.
pub fn hash_goal(goal: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    goal.hash(&mut hasher);
    hasher.finish()
}

/// Combined Pantograph + proof tree state, shared across agent and search.
pub struct SessionProver {
    pub pantograph: Pantograph,
    pub tree: ProofTreeState,
}

/// Thread-safe handle to the shared prover.
pub type SharedProver = Arc<Mutex<SessionProver>>;

impl SessionProver {
    /// Spawn a new Pantograph REPL and create empty proof tree state.
    /// Takes ~18s for Mathlib import (one-time cost per session).
    pub fn spawn(project_dir: &Path) -> Result<Self> {
        let pantograph = Pantograph::spawn(project_dir)?;
        Ok(Self {
            pantograph,
            tree: ProofTreeState::default(),
        })
    }

    /// Check if the Pantograph process is still alive.
    pub fn is_alive(&mut self) -> bool {
        self.pantograph.is_alive()
    }
}
