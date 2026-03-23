//! LRU tactic cache: avoid re-screening identical (goal, tactic) pairs.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use openproof_lean::goal_state::AttemptResult;

/// Maximum cache entries before eviction.
const MAX_ENTRIES: usize = 4096;

/// Cache key: hash of (goal_text, tactic).
type CacheKey = u64;

/// LRU cache for tactic screening results.
pub struct TacticCache {
    entries: HashMap<CacheKey, CacheEntry>,
    access_order: Vec<CacheKey>,
}

struct CacheEntry {
    result: AttemptResult,
}

impl TacticCache {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            access_order: Vec::new(),
        }
    }

    /// Look up a cached result for a (goal, tactic) pair.
    pub fn get(&mut self, goal_text: &str, tactic: &str) -> Option<&AttemptResult> {
        let key = hash_key(goal_text, tactic);
        if self.entries.contains_key(&key) {
            // Move to end of access order (most recently used)
            self.access_order.retain(|k| *k != key);
            self.access_order.push(key);
            Some(&self.entries[&key].result)
        } else {
            None
        }
    }

    /// Insert a result for a (goal, tactic) pair.
    pub fn insert(&mut self, goal_text: &str, tactic: &str, result: AttemptResult) {
        let key = hash_key(goal_text, tactic);

        // Evict oldest if at capacity
        while self.entries.len() >= MAX_ENTRIES {
            if let Some(oldest) = self.access_order.first().copied() {
                self.access_order.remove(0);
                self.entries.remove(&oldest);
            } else {
                break;
            }
        }

        self.entries.insert(key, CacheEntry { result });
        self.access_order.retain(|k| *k != key);
        self.access_order.push(key);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for TacticCache {
    fn default() -> Self {
        Self::new()
    }
}

fn hash_key(goal_text: &str, tactic: &str) -> CacheKey {
    let mut hasher = DefaultHasher::new();
    goal_text.hash(&mut hasher);
    tactic.hash(&mut hasher);
    hasher.finish()
}

/// Compute a hash for a set of goal strings (used for transposition table).
pub fn hash_goals(goals: &[String]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for goal in goals {
        goal.hash(&mut hasher);
    }
    hasher.finish()
}
