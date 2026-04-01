//! Ollama client for model-based tactic proposals.
//!
//! Calls a local ollama instance running BFS-Prover-V2-7B (or compatible)
//! to generate goal-conditioned tactic candidates for best-first search.
//!
//! Prompt format: `{tactic_state}:::` -> model generates a single tactic.

use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

/// Default ollama endpoint.
const DEFAULT_URL: &str = "http://localhost:11434";

/// Default model name for tactic generation.
const DEFAULT_MODEL: &str = "hf.co/mradermacher/BFS-Prover-V2-7B-GGUF:Q4_K_M";

/// Max tokens per tactic (tactics are short).
const MAX_TOKENS: u32 = 256;

/// Tactics that should never be proposed (unsound or buggy).
const BANNED_TACTICS: &[&str] = &["sorry", "admit", "native_decide"];

/// Banned substrings in tactic output.
const BANNED_SUBSTRINGS: &[&str] = &["?_"];

#[derive(Debug, Deserialize)]
struct OllamaResponse {
    response: Option<String>,
    #[allow(dead_code)]
    done: Option<bool>,
}

/// Client for generating tactic proposals via a local ollama server.
pub struct OllamaProposer {
    client: reqwest::blocking::Client,
    url: String,
    model: String,
    temperature: f64,
    top_p: f64,
}

impl Default for OllamaProposer {
    fn default() -> Self {
        Self::new()
    }
}

impl OllamaProposer {
    /// Create a new proposer with default settings.
    pub fn new() -> Self {
        Self {
            client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("failed to build HTTP client"),
            url: DEFAULT_URL.to_string(),
            model: DEFAULT_MODEL.to_string(),
            temperature: 0.8,
            top_p: 0.95,
        }
    }

    /// Create with a custom model name and URL.
    pub fn with_model(model: &str, url: &str) -> Self {
        Self {
            model: model.to_string(),
            url: url.to_string(),
            ..Self::new()
        }
    }

    /// Check if ollama is reachable and the model is available.
    pub fn is_available(&self) -> bool {
        self.client
            .get(format!("{}/api/tags", self.url))
            .timeout(Duration::from_secs(3))
            .send()
            .is_ok()
    }

    /// Generate a single tactic for a goal state.
    fn generate_one(&self, goal_state: &str) -> Result<Option<String>> {
        let prompt = format!("{}:::", goal_state);

        let body = serde_json::json!({
            "model": self.model,
            "prompt": prompt,
            "stream": false,
            "options": {
                "temperature": self.temperature,
                "top_p": self.top_p,
                "num_predict": MAX_TOKENS,
                "stop": ["\n\n", ":::"],
            }
        });

        let resp: OllamaResponse = self
            .client
            .post(format!("{}/api/generate", self.url))
            .json(&body)
            .send()
            .context("ollama request failed")?
            .json()
            .context("ollama response parse failed")?;

        let tactic = resp
            .response
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        Ok(tactic.and_then(|t| filter_tactic(&t)))
    }

    /// Generate `k` tactic candidates for a goal state.
    /// Makes `k` sequential calls with temperature sampling for diversity.
    pub fn propose_tactics(&self, goal_state: &str, k: usize) -> Result<Vec<String>> {
        let mut tactics = Vec::with_capacity(k);
        let mut seen = std::collections::HashSet::new();

        // Make up to 2*k attempts to get k unique tactics
        for _ in 0..(k * 2) {
            if tactics.len() >= k {
                break;
            }
            match self.generate_one(goal_state) {
                Ok(Some(tactic)) => {
                    if seen.insert(tactic.clone()) {
                        tactics.push(tactic);
                    }
                }
                Ok(None) => continue,
                Err(_) => break, // ollama down, stop trying
            }
        }

        Ok(tactics)
    }
}

/// Filter out banned tactics and clean up model output.
pub fn filter_tactic(raw: &str) -> Option<String> {
    let tactic = raw
        .lines()
        .next()
        .unwrap_or(raw)
        .trim()
        .trim_end_matches(":::") // model sometimes echoes separator
        .trim()
        .to_string();

    if tactic.is_empty() {
        return None;
    }

    // Check banned tactics (as whole word at start)
    let lower = tactic.to_lowercase();
    for banned in BANNED_TACTICS {
        if lower == *banned
            || lower.starts_with(&format!("{} ", banned))
            || lower.starts_with(&format!("{};", banned))
        {
            return None;
        }
    }

    // Check banned substrings
    for sub in BANNED_SUBSTRINGS {
        if tactic.contains(sub) {
            return None;
        }
    }

    Some(tactic)
}

/// Build a `ProposeFn` that uses ollama for model-based proposals,
/// falling back to the provided standard tactics when the model is
/// unavailable or returns fewer than `k` candidates.
pub fn make_model_propose_fn(
    proposer: OllamaProposer,
    fallback_tactics: Vec<String>,
) -> crate::search::ProposeFn {
    Box::new(move |goal: &str, _context: &str, k: usize| {
        let mut candidates = Vec::with_capacity(k);

        // Try model first
        if let Ok(model_tactics) = proposer.propose_tactics(goal, k) {
            candidates.extend(model_tactics);
        }

        // Fill remaining slots with standard tactics
        let mut seen: std::collections::HashSet<String> = candidates.iter().cloned().collect();
        for t in &fallback_tactics {
            if candidates.len() >= k {
                break;
            }
            if seen.insert(t.clone()) {
                candidates.push(t.clone());
            }
        }

        candidates.truncate(k);
        Ok(candidates)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_tactic_basic() {
        assert_eq!(filter_tactic("simp"), Some("simp".to_string()));
        assert_eq!(filter_tactic("  omega  "), Some("omega".to_string()));
        assert_eq!(filter_tactic("sorry"), None);
        assert_eq!(filter_tactic("admit"), None);
        assert_eq!(filter_tactic("native_decide"), None);
        assert_eq!(filter_tactic(""), None);
        assert_eq!(filter_tactic("sorry; ring"), None);
    }

    #[test]
    fn test_filter_tactic_banned_substrings() {
        assert_eq!(filter_tactic("rcases h with ?_ | ?_"), None);
        assert_eq!(
            filter_tactic("rcases h with h1 | h2"),
            Some("rcases h with h1 | h2".to_string())
        );
    }

    #[test]
    fn test_filter_tactic_multiline() {
        // Model might generate multi-line; take only first line
        assert_eq!(filter_tactic("ring\n  -- done"), Some("ring".to_string()));
    }

    #[test]
    fn test_filter_tactic_separator() {
        assert_eq!(filter_tactic("omega:::"), Some("omega".to_string()));
    }
}
