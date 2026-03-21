//! Miscellaneous helper utilities shared across all modules.
//!
//! Covers: project-path resolution, string utilities, branch scoring helpers,
//! agent-role / share-mode label functions, and the `persist_write` /
//! `emit_local_notice` plumbing helpers.
//!
//! Paper rendering, file I/O, and export are in `export.rs`.

use anyhow::Result;
use openproof_core::{AppEvent, AppState, PendingWrite};
use openproof_protocol::{AgentRole, ProofBranch, SessionSnapshot, ShareMode};
use openproof_store::AppStore;
use std::{env, path::{Path, PathBuf}};
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// Persist plumbing
// ---------------------------------------------------------------------------

pub fn persist_write(
    tx: mpsc::UnboundedSender<AppEvent>,
    store: AppStore,
    write: PendingWrite,
) {
    let session_id = write.session.id.clone();
    tokio::spawn(async move {
        let outcome = tokio::task::spawn_blocking(move || store.save_session(&write.session))
            .await
            .ok()
            .and_then(Result::ok);
        match outcome {
            Some(_) => {
                let _ = tx.send(AppEvent::PersistSucceeded(session_id));
            }
            None => {
                let _ = tx.send(AppEvent::PersistFailed("store save failed".to_string()));
            }
        }
    });
}

pub fn emit_local_notice(
    tx: mpsc::UnboundedSender<AppEvent>,
    state: &mut AppState,
    store: AppStore,
    title: &str,
    content: String,
) {
    if let Some(write) = state.apply(AppEvent::AppendNotice {
        title: title.to_string(),
        content,
    }) {
        persist_write(tx, store, write);
    }
}

pub fn persist_current_session(
    tx: mpsc::UnboundedSender<AppEvent>,
    store: AppStore,
    state: &mut AppState,
    status: impl Into<String>,
) {
    let Some(session) = state.current_session().cloned() else {
        return;
    };
    state.pending_writes += 1;
    state.status = status.into();
    persist_write(tx, store, PendingWrite { session });
}

// ---------------------------------------------------------------------------
// Lean project resolution
// ---------------------------------------------------------------------------

pub fn is_lean_project_dir(dir: &Path) -> bool {
    dir.join("lakefile.lean").exists() || dir.join("lakefile.toml").exists()
}

pub fn resolve_lean_project_dir() -> PathBuf {
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if is_lean_project_dir(&cwd) {
        return cwd;
    }
    let lean_sub = cwd.join("lean");
    if is_lean_project_dir(&lean_sub) {
        return lean_sub;
    }
    if let Ok(launch) = env::var("OPENPROOF_LAUNCH_CWD") {
        let launch_lean = PathBuf::from(&launch).join("lean");
        if is_lean_project_dir(&launch_lean) {
            return launch_lean;
        }
    }
    lean_sub
}

// ---------------------------------------------------------------------------
// Lean text helpers
// ---------------------------------------------------------------------------

pub fn extract_lean_blocks_from_text(content: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut rest = content;
    while let Some(start) = rest.find("```lean") {
        let after = &rest[start + "```lean".len()..];
        let after = after.strip_prefix('\n').unwrap_or(after);
        let Some(end) = after.find("```") else { break };
        let block = after[..end].trim();
        if !block.is_empty() {
            blocks.push(block.to_string());
        }
        rest = &after[end + 3..];
    }
    blocks
}

// ---------------------------------------------------------------------------
// Branch helpers
// ---------------------------------------------------------------------------

pub fn best_hidden_branch(session: &SessionSnapshot) -> Option<&ProofBranch> {
    session
        .proof
        .branches
        .iter()
        .filter(|branch| branch.hidden)
        .max_by(|left, right| {
            left.score
                .partial_cmp(&right.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.updated_at.cmp(&right.updated_at))
        })
}

pub fn current_foreground_branch(session: Option<&SessionSnapshot>) -> Option<&ProofBranch> {
    let session = session?;
    session
        .proof
        .active_foreground_branch_id
        .as_deref()
        .and_then(|branch_id| {
            session
                .proof
                .branches
                .iter()
                .find(|branch| branch.id == branch_id)
        })
}

pub fn should_promote_hidden_branch(
    candidate: Option<ProofBranch>,
    current: Option<ProofBranch>,
) -> bool {
    let Some(candidate) = candidate else {
        return false;
    };
    let Some(current) = current else {
        return true;
    };
    if candidate.score >= 100.0 && current.score < 100.0 {
        return true;
    }
    if candidate.score > current.score + 12.0 {
        return true;
    }
    let candidate_has_diag = candidate
        .latest_diagnostics
        .as_ref()
        .map(|item| !item.trim().is_empty())
        .unwrap_or(false)
        || !candidate.last_lean_diagnostic.trim().is_empty();
    let current_has_diag = current
        .latest_diagnostics
        .as_ref()
        .map(|item| !item.trim().is_empty())
        .unwrap_or(false)
        || !current.last_lean_diagnostic.trim().is_empty();
    !candidate_has_diag && current_has_diag
}

pub fn autonomous_stop_reason(session: &SessionSnapshot) -> Option<String> {
    use openproof_protocol::AgentStatus;

    if session.proof.pending_question.is_some() || session.proof.awaiting_clarification {
        return Some("Autonomous loop paused for clarification.".to_string());
    }
    if session.proof.phase == "done" {
        return Some("Autonomous loop completed the current proof run.".to_string());
    }
    if session.proof.phase == "blocked" {
        return Some("Autonomous loop paused on a blocker.".to_string());
    }
    if session.proof.accepted_target.is_none() && session.proof.formal_target.is_none() {
        return Some(
            "Set or accept a formal target before running autonomous search.".to_string(),
        );
    }
    let all_finished = !session.proof.branches.is_empty()
        && session
            .proof
            .branches
            .iter()
            .all(|branch| branch.status != AgentStatus::Running);
    let all_stalled = all_finished
        && session.proof.autonomous_iteration_count >= 6
        && session.proof.branches.iter().all(|branch| {
            matches!(
                branch.status,
                AgentStatus::Blocked | AgentStatus::Done | AgentStatus::Error
            )
        });
    if all_stalled {
        return Some("Autonomous loop paused after low-progress iterations.".to_string());
    }
    None
}

// ---------------------------------------------------------------------------
// Agent / role label helpers
// ---------------------------------------------------------------------------

pub fn agent_role_label(role: AgentRole) -> &'static str {
    match role {
        AgentRole::Planner => "planner",
        AgentRole::Prover => "prover",
        AgentRole::Repairer => "repairer",
        AgentRole::Retriever => "retriever",
        AgentRole::Critic => "critic",
    }
}

pub fn branch_phase_for_role(role: AgentRole) -> &'static str {
    match role {
        AgentRole::Planner => "planning",
        AgentRole::Retriever => "retrieving",
        AgentRole::Prover => "proving",
        AgentRole::Repairer => "repairing",
        AgentRole::Critic => "blocked",
    }
}

pub fn parse_agent_role(value: &str) -> Option<AgentRole> {
    match value.trim() {
        "planner" => Some(AgentRole::Planner),
        "prover" => Some(AgentRole::Prover),
        "repairer" => Some(AgentRole::Repairer),
        "retriever" => Some(AgentRole::Retriever),
        "critic" => Some(AgentRole::Critic),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Share mode helpers
// ---------------------------------------------------------------------------

pub fn share_mode_label(mode: ShareMode) -> &'static str {
    match mode {
        ShareMode::Local => "local",
        ShareMode::Community => "community",
        ShareMode::Private => "private",
    }
}

pub fn parse_share_mode(value: &str) -> Option<ShareMode> {
    match value.trim() {
        "local" => Some(ShareMode::Local),
        "community" => Some(ShareMode::Community),
        "private" => Some(ShareMode::Private),
        _ => None,
    }
}

pub fn describe_remote_corpus() -> String {
    let client = openproof_cloud::CloudCorpusClient::new(Default::default());
    client.describe()
}

// ---------------------------------------------------------------------------
// String utilities
// ---------------------------------------------------------------------------

pub fn truncate(input: &str, limit: usize) -> String {
    let trimmed = input.trim();
    if trimmed.chars().count() <= limit {
        return trimmed.to_string();
    }
    trimmed
        .chars()
        .take(limit.saturating_sub(1))
        .collect::<String>()
        + "…"
}

pub fn summarize_branch_output(content: &str) -> String {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("NEXT:") {
            let value = value.trim();
            if !value.is_empty() {
                return value.to_string();
            }
        }
        if let Some(value) = trimmed.strip_prefix("STATUS:") {
            let value = value.trim();
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }
    truncate(content, 160)
}

pub fn parse_statement_args(arg_text: &str) -> Option<(String, String)> {
    let (label, statement) = arg_text.split_once("::")?;
    let label = label.trim();
    let statement = statement.trim();
    if label.is_empty() || statement.is_empty() {
        None
    } else {
        Some((label.to_string(), statement.to_string()))
    }
}

/// Embed a verified corpus item into the vector store (fire-and-forget).
/// Runs in background -- does not block the caller. Silently skips if
/// Qdrant is unavailable or OPENAI_API_KEY is not set.
pub fn embed_verified_item(
    identity_key: String,
    label: String,
    statement: String,
    decl_kind: String,
    module_name: String,
    artifact_content: String,
) {
    tokio::spawn(async move {
        use openproof_store::embeddings::{build_embedding_text, generate_embedding, EmbeddingStore};
        let text = build_embedding_text(&label, &statement, &decl_kind, &module_name, &artifact_content);
        let Some(embedding) = generate_embedding(&text).await else {
            return; // No API key or API error -- skip silently
        };
        let store = match EmbeddingStore::open_remote("http://localhost:6334").await {
            Ok(s) => s,
            Err(_) => return, // Qdrant not running -- skip silently
        };
        let _ = store
            .upsert_item(&identity_key, &label, &statement, &decl_kind, &module_name, &artifact_content, embedding)
            .await;
    });
}
