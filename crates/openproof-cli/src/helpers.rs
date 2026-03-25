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
    // Paper.tex is a workspace file managed by the model via file_write/file_patch.
    // Read it from the workspace on every save so the dashboard has the latest.
    let mut session = write.session;
    let ws_dir = store.workspace_dir(&session.id);
    if let Ok(paper) = std::fs::read_to_string(ws_dir.join("Paper.tex")) {
        if !paper.trim().is_empty() {
            session.proof.paper_tex = paper;
        }
    }
    let write = PendingWrite { session };
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

/// Check if the autonomous loop should stop.
/// `full_autonomous` = true means never stop unless user interrupts or there's
/// literally nothing to do. `false` = current behavior (stop on done/blocked/stall).
pub fn autonomous_stop_reason(session: &SessionSnapshot) -> Option<String> {
    autonomous_stop_reason_with_mode(session, false)
}

pub fn autonomous_stop_reason_with_mode(
    session: &SessionSnapshot,
    full_autonomous: bool,
) -> Option<String> {
    use openproof_protocol::AgentStatus;

    // Always stop for clarification questions -- need user input
    if session.proof.pending_question.is_some() || session.proof.awaiting_clarification {
        return Some("Paused for clarification.".to_string());
    }

    // Always need a target
    if session.proof.accepted_target.is_none() && session.proof.formal_target.is_none() {
        return Some(
            "Set or accept a formal target before running autonomous search.".to_string(),
        );
    }

    if full_autonomous {
        // Full autonomous: only stop if ALL nodes are verified (proof complete)
        let all_verified = !session.proof.nodes.is_empty()
            && session.proof.nodes.iter().all(|n| {
                n.status == openproof_protocol::ProofNodeStatus::Verified
            });
        if all_verified {
            return Some("All proof nodes verified.".to_string());
        }
        // Never stop otherwise -- keep going
        return None;
    }

    // Normal mode: stop on done/blocked/stall
    if session.proof.phase == "done" {
        return Some("Completed the current proof run.".to_string());
    }
    if session.proof.phase == "blocked" {
        return Some("Paused on a blocker.".to_string());
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
        return Some("Paused after low-progress iterations.".to_string());
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

/// Auto-tag a verified corpus item by domain and record graph metadata.
pub fn index_verified_item(store: AppStore, identity_key: String, module_name: String) {
    tokio::spawn(async move {
        let store_ref = store.clone();
        let ik = identity_key.clone();
        let mn = module_name.clone();
        let _ = tokio::task::spawn_blocking(move || {
            store_ref.auto_tag_from_module(&ik, &mn)
        }).await;
    });
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

/// Index verified declarations: extract dependency edges and domain tags.
/// Called after successful verification.
pub fn populate_knowledge_graph(store: &AppStore, session_id: &str) {
    let ws_dir = store.workspace_dir(session_id);
    let mut all_lean = String::new();
    if let Ok(files) = store.list_workspace_files(session_id) {
        for (path, _) in &files {
            if path.ends_with(".lean") && !path.contains("history/") {
                if let Ok(content) = std::fs::read_to_string(ws_dir.join(path)) {
                    if !all_lean.is_empty() { all_lean.push_str("\n\n"); }
                    all_lean.push_str(&content);
                }
            }
        }
    }
    if all_lean.trim().is_empty() {
        return;
    }
    let parsed = openproof_lean::parse_lean_declarations(&all_lean);
    let all_names: Vec<&str> = parsed.iter().map(|d| d.name.as_str()).collect();
    // Build identity keys matching the format used by record_verification_result:
    // user-verified/{session_id}/{label}/{hash_of_signature}
    let key_for = |name: &str, sig: &str| -> String {
        format!("user-verified/{}/{}/{}",
            openproof_store::sanitize_identity_segment(session_id),
            openproof_store::sanitize_identity_segment(name),
            openproof_store::corpus_hash(sig),
        )
    };
    // Build a lookup from name -> (name, signature)
    let sig_map: std::collections::HashMap<&str, &str> = parsed.iter()
        .map(|d| (d.name.as_str(), d.signature.as_str())).collect();
    for decl in &parsed {
        let from_key = key_for(&decl.name, &decl.signature);
        for dep in openproof_lean::parse::extract_dependencies(&decl.body, &all_names, &decl.name) {
            if let Some(&dep_sig) = sig_map.get(dep.as_str()) {
                let to_key = key_for(&dep, dep_sig);
                let _ = store.add_corpus_edge(&from_key, &to_key, "uses", 1.0);
            }
        }
        let _ = store.auto_tag_from_module(&from_key, &decl.name);
    }
}

/// Generate a LaTeX paper body from the current proof state.
/// Called on every session save so the paper is always up to date.
pub fn generate_paper_tex(
    title: &str,
    problem: &str,
    nodes: &[openproof_protocol::ProofNode],
) -> String {
    if nodes.is_empty() {
        return String::new();
    }
    let mut tex = String::new();
    tex.push_str(&format!("\\section{{{}}}\n\n", escape_latex(title)));
    if !problem.is_empty() {
        tex.push_str(&format!("\\subsection*{{Problem}}\n{}\n\n", escape_latex(problem)));
    }
    for node in nodes {
        let env = match node.kind {
            openproof_protocol::ProofNodeKind::Theorem => "theorem",
            openproof_protocol::ProofNodeKind::Lemma => "lemma",
            _ => "proposition",
        };
        let status_str = match node.status {
            openproof_protocol::ProofNodeStatus::Verified => "Verified in Lean 4.",
            openproof_protocol::ProofNodeStatus::Failed => "Proof attempt failed.",
            openproof_protocol::ProofNodeStatus::Proving => "Proof in progress.",
            _ => "Pending.",
        };
        tex.push_str(&format!(
            "\\begin{{{env}}}[{}]\n{}\n\\end{{{env}}}\n",
            escape_latex(&node.label),
            escape_latex(&node.statement),
        ));
        tex.push_str(&format!("\\textit{{{status_str}}}\n\n"));
        if !node.content.trim().is_empty() {
            tex.push_str("\\begin{lstlisting}[language=Lean]\n");
            let code = if node.content.len() > 3000 {
                &node.content.chars().take(3000).collect::<String>()
            } else {
                &node.content
            };
            tex.push_str(code);
            if node.content.len() > 3000 {
                tex.push_str("\n% ... (truncated)");
            }
            tex.push_str("\n\\end{lstlisting}\n\n");
        }
    }
    tex
}

fn escape_latex(s: &str) -> String {
    s.replace('\\', "\\textbackslash{}")
        .replace('{', "\\{")
        .replace('}', "\\}")
        .replace('&', "\\&")
        .replace('%', "\\%")
        .replace('$', "\\$")
        .replace('#', "\\#")
        .replace('_', "\\_")
        .replace('~', "\\textasciitilde{}")
        .replace('^', "\\textasciicircum{}")
}
