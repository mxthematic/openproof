//! MLX-based tactic proposer for Apple Silicon.
//!
//! Connects to `mlx_lm.server` which serves an OpenAI-compatible HTTP API.
//! Uses `n=k` to get all candidates in a single request (shared prompt encoding).

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashSet;
use std::time::Duration;

use crate::ollama::filter_tactic;

const DEFAULT_PORT: u16 = 8321;
const DEFAULT_MODEL_DIR: &str = ".openproof/models/openproof-tactic-2b";
const MAX_TOKENS: usize = 256;
const REQUEST_TIMEOUT_SECS: u64 = 30;

/// MLX tactic proposer -- talks to `mlx_lm.server` via OpenAI completions API.
pub struct MlxProposer {
    client: reqwest::blocking::Client,
    url: String,
    model_path: String,
    temperature: f64,
    top_p: f64,
}

#[derive(Deserialize)]
struct CompletionChoice {
    text: String,
}

#[derive(Deserialize)]
struct CompletionResponse {
    choices: Vec<CompletionChoice>,
}

impl Default for MlxProposer {
    fn default() -> Self {
        Self::new()
    }
}

impl MlxProposer {
    /// Create a proposer with default settings.
    pub fn new() -> Self {
        let home = directories::BaseDirs::new()
            .map(|d| d.home_dir().to_path_buf())
            .unwrap_or_default();
        let model_path = home.join(DEFAULT_MODEL_DIR).display().to_string();
        Self::with_config(&model_path, DEFAULT_PORT)
    }

    /// Create a proposer with custom model path and port.
    pub fn with_config(model_path: &str, port: u16) -> Self {
        Self {
            client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
                .build()
                .unwrap_or_default(),
            url: format!("http://localhost:{port}"),
            model_path: model_path.to_string(),
            temperature: 0.8,
            top_p: 0.95,
        }
    }

    /// Check if the MLX server is running and responsive.
    pub fn is_available(&self) -> bool {
        self.client
            .get(format!("{}/v1/models", self.url))
            .timeout(Duration::from_secs(2))
            .send()
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    /// Path to the MLX model directory.
    pub fn model_path(&self) -> &str {
        &self.model_path
    }

    /// Port the server should run on.
    pub fn port(&self) -> u16 {
        self.url
            .rsplit(':')
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_PORT)
    }

    /// Generate a single tactic for a goal state.
    fn generate_one(&self, goal_state: &str) -> Result<Option<String>> {
        let prompt = format!("{goal_state}:::");

        let body = serde_json::json!({
            "prompt": prompt,
            "max_tokens": MAX_TOKENS,
            "temperature": self.temperature,
            "top_p": self.top_p,
            "stop": ["\n\n", ":::"],
        });

        let resp: CompletionResponse = self
            .client
            .post(format!("{}/v1/completions", self.url))
            .json(&body)
            .send()
            .context("MLX server request failed")?
            .json()
            .context("MLX response parse failed")?;

        let tactic = resp
            .choices
            .first()
            .map(|c| c.text.trim().to_string())
            .filter(|s| !s.is_empty());

        Ok(tactic.and_then(|t| filter_tactic(&t)))
    }

    /// Generate `k` tactic candidates for a goal state.
    /// Makes up to `2*k` sequential calls with temperature sampling for diversity.
    pub fn propose_tactics(&self, goal_state: &str, k: usize) -> Result<Vec<String>> {
        let mut tactics = Vec::with_capacity(k);
        let mut seen = HashSet::new();

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
                Err(_) => break,
            }
        }

        Ok(tactics)
    }
}

/// Build a `ProposeFn` that uses MLX for model-based proposals,
/// falling back to the provided standard tactics.
pub fn make_mlx_propose_fn(
    proposer: MlxProposer,
    fallback_tactics: Vec<String>,
) -> crate::search::ProposeFn {
    Box::new(move |goal: &str, _context: &str, k: usize| {
        let mut candidates = Vec::with_capacity(k);

        if let Ok(model_tactics) = proposer.propose_tactics(goal, k) {
            candidates.extend(model_tactics);
        }

        let mut seen: HashSet<String> = candidates.iter().cloned().collect();
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

/// Check if an MLX model is installed at the default location.
pub fn mlx_model_exists() -> bool {
    let home = directories::BaseDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .unwrap_or_default();
    home.join(DEFAULT_MODEL_DIR).join("config.json").exists()
}

/// Default model path.
pub fn default_model_path() -> String {
    let home = directories::BaseDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .unwrap_or_default();
    home.join(DEFAULT_MODEL_DIR).display().to_string()
}
