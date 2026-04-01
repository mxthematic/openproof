//! Batch generation of tactic proposals using Codex.
//!
//! Reads goal states from stdin (one per line), sends each to Codex asking for
//! smart goal-specific tactics, writes unverified pairs to stdout as JSONL.
//!
//! Usage:
//!   openproof expert-gen < goals.txt > unverified_pairs.jsonl

use anyhow::Result;
use openproof_model::{CodexTurnRequest, TurnMessage};
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, Write};
use std::time::Duration;

const SYSTEM_PROMPT: &str = "\
You are an expert Lean 4 tactic advisor. Given a goal state, propose 1-5 specific tactics \
that would CLOSE the goal entirely (not just simplify it). Think carefully about the goal \
structure. Use specific Mathlib lemma names when appropriate. Prefer concrete tactics over \
generic ones -- e.g. 'nlinarith [sq_nonneg (a - b)]' over just 'nlinarith'. \
For algebraic goals, use ring/linarith with specific witnesses. \
For logical goals, use exact/apply with specific lemma names. \
Reply with ONLY a JSON object: {\"tactics\":[\"tactic1\",\"tactic2\"]}. \
No markdown, no explanation. Never use sorry, admit, or native_decide.";

const DEFAULT_MODEL: &str = "gpt-5.4";

#[derive(Serialize)]
struct UnverifiedPair {
    goal_state: String,
    proposed_tactic: String,
}

#[derive(Deserialize)]
struct TacticResponse {
    tactics: Vec<String>,
}

fn parse_tactics(response: &str) -> Vec<String> {
    // Try direct parse
    if let Ok(parsed) = serde_json::from_str::<TacticResponse>(response) {
        return parsed.tactics;
    }

    // Try stripping markdown
    let trimmed = response
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if let Ok(parsed) = serde_json::from_str::<TacticResponse>(trimmed) {
        return parsed.tactics;
    }

    // Try finding JSON in text
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            if let Ok(parsed) = serde_json::from_str::<TacticResponse>(&trimmed[start..=end]) {
                return parsed.tactics;
            }
        }
    }

    Vec::new()
}

pub async fn run_expert_gen() -> Result<()> {
    let model =
        std::env::var("OPENPROOF_TACTIC_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());

    // Sync auth
    openproof_model::sync_auth_from_codex_cli()?;

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());

    let mut total = 0usize;
    let mut generated = 0usize;
    let mut errors = 0usize;

    for line in stdin.lock().lines() {
        let goal = match line {
            Ok(l) => l.trim().to_string(),
            Err(_) => break,
        };
        if goal.is_empty() {
            continue;
        }
        total += 1;

        let prompt = format!("Goal state:\n{goal}");
        let messages = vec![
            TurnMessage::chat("system", SYSTEM_PROMPT),
            TurnMessage::chat("user", prompt),
        ];
        let session_id = format!("expert-gen-{}", chrono::Utc::now().timestamp_millis());
        let request = CodexTurnRequest {
            session_id: &session_id,
            messages: &messages,
            model: &model,
            reasoning_effort: "low",
            include_tools: false,
        };

        match tokio::time::timeout(
            Duration::from_secs(300),
            openproof_model::run_codex_turn(request),
        )
        .await
        {
            Ok(Ok(response)) => {
                let tactics = parse_tactics(&response);
                for tactic in &tactics {
                    let tactic = tactic.trim();
                    if tactic.is_empty() {
                        continue;
                    }
                    let lower = tactic.to_lowercase();
                    if lower == "sorry" || lower == "admit" || lower == "native_decide" {
                        continue;
                    }
                    let pair = UnverifiedPair {
                        goal_state: goal.clone(),
                        proposed_tactic: tactic.to_string(),
                    };
                    if let Ok(json) = serde_json::to_string(&pair) {
                        let _ = writeln!(out, "{json}");
                    }
                    generated += 1;
                }
                if tactics.is_empty() {
                    errors += 1;
                }
            }
            Ok(Err(e)) => {
                eprintln!("[expert-gen] Error on goal {total}: {e}");
                errors += 1;
            }
            Err(_) => {
                eprintln!("[expert-gen] Timeout on goal {total}");
                errors += 1;
            }
        }

        #[allow(clippy::manual_is_multiple_of)]
        if total % 50 == 0 {
            eprintln!(
                "[expert-gen] {total} goals processed, {generated} tactics generated, {errors} errors"
            );
        }
    }

    let _ = out.flush();
    eprintln!("[expert-gen] Done: {total} goals, {generated} tactics, {errors} errors");
    Ok(())
}
