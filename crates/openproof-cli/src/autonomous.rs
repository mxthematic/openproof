//! Autonomous proof-search loop — TUI-driven functions.
//!
//! `schedule_autonomous_tick` and `run_autonomous_step` drive the
//! autonomous loop inside the interactive TUI via `AppEvent::AutonomousTick`.
//!
//! The headless CLI entry point (`openproof run`) lives in
//! `autonomous_headless.rs`.

use crate::helpers::{
    autonomous_stop_reason_with_mode, best_hidden_branch, current_foreground_branch,
    persist_current_session, persist_write, should_promote_hidden_branch,
};
use crate::turn_handling::{
    ensure_hidden_agent_branch, start_agent_branch_turn, start_branch_verification,
};
use anyhow::{Context, Result};
use directories::BaseDirs;
use openproof_core::{AppEvent, AppState, AutonomousRunPatch};
use openproof_lean::lsp_mcp::LeanLspMcp;
use openproof_model::{CodexTurnRequest, TurnMessage};
use openproof_protocol::{AgentRole, AgentStatus, BranchQueueState, SearchStrategy};
use openproof_search::config::TacticSearchConfig;
use openproof_search::lsp_search::best_first_search;
use openproof_store::{AppStore, StorePaths};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::mpsc;

const DEFAULT_CODEX_TACTIC_MODEL: &str = "gpt-5.4";
const DEFAULT_CODEX_REASONING_EFFORT: &str = "low";
const MAX_CODEX_TACTIC_CANDIDATES: usize = 3;

static EXPERT_DATA_WRITE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TacticProposerBackend {
    Standard,
    Ollama,
    Codex,
}

impl TacticProposerBackend {
    fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Ollama => "ollama",
            Self::Codex => "codex",
        }
    }
}

#[derive(Debug, Deserialize)]
struct CodexTacticResponse {
    tactics: Vec<String>,
}

#[derive(Debug, Clone)]
struct ExpertExportContext {
    session_id: String,
    node_id: String,
    node_label: String,
    node_statement: String,
    backend: String,
    model: Option<String>,
}

#[derive(Debug, Serialize)]
struct ExpertPositiveRecord {
    prompt: String,
    completion: String,
    goal_state: String,
    step_index: usize,
    session_id: String,
    node_id: String,
    node_label: String,
    node_statement: String,
    sorry_line: usize,
    backend: String,
    model: Option<String>,
    created_at: String,
}

#[derive(Debug, Serialize)]
struct ExpertTrajectoryRecord {
    root_goal: String,
    goals_before: Vec<String>,
    tactics: Vec<String>,
    session_id: String,
    node_id: String,
    node_label: String,
    node_statement: String,
    sorry_line: usize,
    backend: String,
    model: Option<String>,
    created_at: String,
}

pub fn schedule_autonomous_tick(
    tx: mpsc::UnboundedSender<AppEvent>,
    store: AppStore,
    state: &mut AppState,
) {
    let Some(session) = state.current_session().cloned() else {
        return;
    };
    if !session.proof.is_autonomous_running {
        return;
    }
    if state.turn_in_flight || state.verification_in_flight {
        return;
    }
    if session
        .proof
        .branches
        .iter()
        .any(|branch| branch.status == AgentStatus::Running)
    {
        return;
    }
    if let Some(reason) = autonomous_stop_reason_with_mode(&session, session.proof.full_autonomous)
    {
        if let Ok(write) = state.set_autonomous_run_state(AutonomousRunPatch {
            is_autonomous_running: Some(false),
            autonomous_pause_reason: Some(Some(reason.clone())),
            autonomous_stop_reason: Some(if session.proof.phase == "done" {
                Some(reason.clone())
            } else {
                None
            }),
            ..AutonomousRunPatch::default()
        }) {
            persist_write(tx, store, write);
        }
        return;
    }
    let _ = run_autonomous_step(tx, store, state);
}

pub fn run_autonomous_step(
    tx: mpsc::UnboundedSender<AppEvent>,
    store: AppStore,
    state: &mut AppState,
) -> Result<String, String> {
    let session = state
        .current_session()
        .cloned()
        .ok_or_else(|| "No active session.".to_string())?;
    if let Some(reason) = autonomous_stop_reason_with_mode(&session, session.proof.full_autonomous)
        .filter(|reason| !reason.contains("All proof nodes verified"))
    {
        return Err(reason);
    }

    // Cycle active_node_id to the next unverified root-level node.
    // This ensures multi-theorem sessions work on each theorem in turn.
    if let Some(s) = state.current_session_mut() {
        let current_verified = s
            .proof
            .active_node_id
            .as_deref()
            .and_then(|id| s.proof.nodes.iter().find(|n| n.id == id))
            .map(|n| n.status == openproof_protocol::ProofNodeStatus::Verified)
            .unwrap_or(true); // treat None as "needs cycling"
        if current_verified || s.proof.active_node_id.is_none() {
            if let Some(next) =
                s.proof.nodes.iter().find(|n| {
                    n.depth == 0 && n.status != openproof_protocol::ProofNodeStatus::Verified
                })
            {
                eprintln!("[auto] Cycling to unverified root: {}", next.label);
                s.proof.active_node_id = Some(next.id.clone());
            }
        }
    }

    // Derive target from active node's statement, with fallback for backward compat
    let active_node = state.current_session().and_then(|s| {
        s.proof
            .active_node_id
            .as_deref()
            .and_then(|id| s.proof.nodes.iter().find(|n| n.id == id))
            .cloned()
    });
    let target = active_node
        .as_ref()
        .filter(|n| !n.statement.trim().is_empty())
        .map(|n| n.statement.clone())
        .or_else(|| session.proof.accepted_target.clone())
        .or_else(|| session.proof.formal_target.clone())
        .ok_or_else(|| {
            "Set or accept a formal target before running autonomous search.".to_string()
        })?;

    // Ensure active_node_id is set (creates a node if none exist)
    if state
        .current_session()
        .map(|s| s.proof.active_node_id.is_none())
        .unwrap_or(false)
    {
        if let Some(s) = state.current_session_mut() {
            if let Some(first) = s.proof.nodes.first() {
                s.proof.active_node_id = Some(first.id.clone());
            } else {
                // No nodes at all -- create one from the target
                eprintln!("[auto] No nodes exist, creating one from target");
                let node = openproof_protocol::ProofNode {
                    id: format!("node_{}", chrono::Utc::now().timestamp_millis()),
                    kind: openproof_protocol::ProofNodeKind::Theorem,
                    label: s.title.clone(),
                    statement: target.clone(),
                    content: String::new(),
                    status: openproof_protocol::ProofNodeStatus::Pending,
                    parent_id: None,
                    depends_on: Vec::new(),
                    depth: 0,
                    created_at: chrono::Utc::now().to_rfc3339(),
                    updated_at: chrono::Utc::now().to_rfc3339(),
                };
                s.proof.active_node_id = Some(node.id.clone());
                s.proof.root_node_id = Some(node.id.clone());
                s.proof.nodes.push(node);
            }
        }
        // Persist the fix
        if let Some(session) = state.current_session().cloned() {
            persist_write(
                tx.clone(),
                store.clone(),
                openproof_core::PendingWrite { session },
            );
        }
    }

    let next_iteration = session.proof.autonomous_iteration_count.saturating_add(1);
    if let Ok(write) = state.set_autonomous_run_state(AutonomousRunPatch {
        autonomous_iteration_count: Some(next_iteration),
        autonomous_pause_reason: Some(None),
        autonomous_stop_reason: Some(None),
        ..AutonomousRunPatch::default()
    }) {
        persist_write(tx.clone(), store.clone(), write);
    }

    let mut actions = Vec::new();
    if let Ok(summary) = refresh_retrieval_branch(tx.clone(), store.clone(), state) {
        actions.push(summary);
    }

    // Always advance past retrieval -- it's informational, not blocking
    if let Some(s) = state.current_session_mut() {
        if s.proof.phase == "retrieving" || s.proof.phase == "idle" {
            s.proof.phase = "proving".to_string();
        }
    }

    let latest_session = state
        .current_session()
        .cloned()
        .ok_or_else(|| "No active session.".to_string())?;

    let best_hidden = best_hidden_branch(&latest_session).cloned();
    let current_fg = current_foreground_branch(Some(&latest_session)).cloned();
    if should_promote_hidden_branch(best_hidden.clone(), current_fg.clone()) {
        if let Some(candidate) = best_hidden {
            let reason = format!("Promoted stronger hidden branch {}.", candidate.title);
            if let Ok(write) =
                state.promote_branch_to_foreground(&candidate.id, false, Some(&reason))
            {
                persist_write(tx.clone(), store.clone(), write);
                actions.push(reason);
            }
        }
    }

    let latest_session = state
        .current_session()
        .cloned()
        .ok_or_else(|| "No active session.".to_string())?;

    // If any node has content but hasn't been verified, verify it now
    // before doing anything else. This closes the loop after tool turns
    // write code via file_write + lean_verify succeeds in the tool loop.
    let unverified_with_content = latest_session.proof.nodes.iter().find(|n| {
        !n.content.trim().is_empty()
            && matches!(
                n.status,
                openproof_protocol::ProofNodeStatus::Pending
                    | openproof_protocol::ProofNodeStatus::Proving
            )
    });
    if let Some(unode) = unverified_with_content {
        eprintln!(
            "[auto] Found unverified node '{}' with content, spawning verification...",
            unode.label
        );
        // Ensure active_node_id points to this node so verify_active_node finds it
        let unode_id = unode.id.clone();
        if let Some(s) = state.current_session_mut() {
            s.proof.active_node_id = Some(unode_id);
        }
        let verification_session = state
            .current_session()
            .cloned()
            .ok_or_else(|| "No active session.".to_string())?;
        if let Some(write) = state.apply(AppEvent::LeanVerifyStarted) {
            persist_write(tx.clone(), store.clone(), write);
        }
        let project_dir = crate::helpers::resolve_lean_project_dir();
        let tx_verify = tx.clone();
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                openproof_lean::verify_active_node(&project_dir, &verification_session)
            })
            .await
            .ok()
            .and_then(|r| r.ok());
            match result {
                Some(summary) => {
                    let _ = tx_verify.send(AppEvent::LeanVerifyFinished(summary));
                }
                None => {
                    let _ = tx_verify.send(AppEvent::AppendNotice {
                        title: "Verify Error".to_string(),
                        content: "Lean verification crashed.".to_string(),
                    });
                }
            }
        });
        actions.push("Started verification of unverified node.".to_string());
        return Ok(actions.join("\n"));
    }

    let repair_basis = current_foreground_branch(Some(&latest_session))
        .filter(|branch| {
            branch
                .latest_diagnostics
                .as_ref()
                .map(|item| !item.trim().is_empty())
                .unwrap_or(false)
                || !branch.last_lean_diagnostic.trim().is_empty()
        })
        .cloned()
        .or_else(|| {
            best_hidden_branch(&latest_session)
                .filter(|branch| {
                    branch
                        .latest_diagnostics
                        .as_ref()
                        .map(|item| !item.trim().is_empty())
                        .unwrap_or(false)
                        || !branch.last_lean_diagnostic.trim().is_empty()
                })
                .cloned()
        });

    if let Some(basis) = repair_basis {
        // SMART DECOMPOSITION: use subtree health scores + BFS metrics
        // to decide when and where to decompose, instead of waiting for
        // a fixed failure count.
        let subtree_scores = crate::decomposition::compute_subtree_scores(
            &latest_session.proof.nodes,
            &latest_session.proof.branches,
        );
        let nogood_ctx = openproof_core::derive_nogood_context(&latest_session.proof.nodes);
        let focus_node = basis
            .focus_node_id
            .as_deref()
            .or(latest_session.proof.active_node_id.as_deref())
            .unwrap_or("");
        let decomp_action =
            crate::decomposition::decide_action(focus_node, &subtree_scores, basis.attempt_count);

        match decomp_action {
            crate::decomposition::DecompositionAction::DecomposeLeaf { node_id } => {
                actions.push(format!(
                    "Decomposing leaf {node_id} (BFS stalled, subtree score {:.2}).",
                    subtree_scores.get(&node_id).map_or(0.0, |s| s.score),
                ));

                let decompose_context = format!(
                    "The proof of {target} is stuck at node {node_id}. \
                     BFS search has stalled after {} attempts.\n\n\
                     DECOMPOSE this goal into 2-4 independent sub-lemmas. For each sub-lemma:\n\
                     1. Emit LEMMA: <label> :: <Lean type signature>\n\
                     2. Give a brief justification of why this sub-lemma helps\n\
                     3. Sketch how the sub-lemmas compose into the final proof\n\n\
                     The sub-lemmas should be EASIER to prove individually than the full goal.\n\
                     Each will be verified independently by Lean.\n\n\
                     Current target: {target}\n\nFailed approach summary:\n{}{nogood_ctx}",
                    basis.attempt_count, basis.summary,
                );

                let title = format!("{} decomposer", latest_session.title);
                let (branch_id, session_snapshot) = ensure_hidden_agent_branch(
                    tx.clone(),
                    store.clone(),
                    state,
                    AgentRole::Planner,
                    &title,
                    "Decompose goal into independent sub-lemmas",
                )?;
                start_agent_branch_turn(
                    tx,
                    store,
                    AgentRole::Planner,
                    decompose_context,
                    branch_id.clone(),
                    branch_id.clone(),
                    session_snapshot,
                );
                actions.push(format!("Started decomposer branch {branch_id}."));
                return Ok(actions.join("\n"));
            }
            crate::decomposition::DecompositionAction::RedecomposeInterior {
                node_id,
                failed_children,
                reason,
            } => {
                actions.push(format!(
                    "Re-decomposing {node_id}: {reason}. Previous decomposition failed.",
                ));

                let decompose_context = format!(
                    "The previous decomposition of {target} has FAILED. {reason}\n\
                     Failed sub-lemmas: [{}]\n\n\
                     The decomposition approach was fundamentally wrong. \
                     Try a COMPLETELY DIFFERENT decomposition strategy.\n\n\
                     DECOMPOSE this goal into 2-4 independent sub-lemmas. For each sub-lemma:\n\
                     1. Emit LEMMA: <label> :: <Lean type signature>\n\
                     2. Give a brief justification of why this sub-lemma helps\n\
                     3. Sketch how the sub-lemmas compose into the final proof\n\n\
                     Current target: {target}\n\nFailed approach summary:\n{}{nogood_ctx}",
                    failed_children.join(", "),
                    basis.summary,
                );

                let title = format!("{} re-decomposer", latest_session.title);
                let (branch_id, session_snapshot) = ensure_hidden_agent_branch(
                    tx.clone(),
                    store.clone(),
                    state,
                    AgentRole::Planner,
                    &title,
                    "Re-decompose goal with a different strategy",
                )?;
                start_agent_branch_turn(
                    tx,
                    store,
                    AgentRole::Planner,
                    decompose_context,
                    branch_id.clone(),
                    branch_id.clone(),
                    session_snapshot,
                );
                actions.push(format!("Started re-decomposer branch {branch_id}."));
                return Ok(actions.join("\n"));
            }
            crate::decomposition::DecompositionAction::FullPivot { reason } => {
                // Fall through to backtracking logic below with a strong signal.
                actions.push(format!("Full strategy pivot: {reason}"));
                // Treat as attempt_count >= 3 to trigger new strategy.
            }
            crate::decomposition::DecompositionAction::Continue => {
                // Check legacy fallback: if attempt_count >= 6, still decompose.
                if basis.attempt_count >= 6 {
                    actions.push(format!(
                        "Decomposing (fallback): {} failed repairs.",
                        basis.attempt_count,
                    ));

                    let decompose_context = format!(
                        "The proof of {target} has failed {} times.\n\n\
                         DECOMPOSE this goal into 2-4 independent sub-lemmas. For each:\n\
                         1. Emit LEMMA: <label> :: <Lean type signature>\n\
                         2. Brief justification\n\
                         3. Sketch how they compose\n\n\
                         Current target: {target}\n\nSummary:\n{}{nogood_ctx}",
                        basis.attempt_count, basis.summary,
                    );

                    let title = format!("{} decomposer", latest_session.title);
                    let (branch_id, session_snapshot) = ensure_hidden_agent_branch(
                        tx.clone(),
                        store.clone(),
                        state,
                        AgentRole::Planner,
                        &title,
                        "Decompose goal into independent sub-lemmas",
                    )?;
                    start_agent_branch_turn(
                        tx,
                        store,
                        AgentRole::Planner,
                        decompose_context,
                        branch_id.clone(),
                        branch_id.clone(),
                        session_snapshot,
                    );
                    actions.push(format!("Started decomposer branch {branch_id}."));
                    return Ok(actions.join("\n"));
                }
            }
        }

        if basis.attempt_count >= 3 {
            actions.push(format!(
                "Backtracking: {} failed repairs on the same approach. Trying a new strategy.",
                basis.attempt_count
            ));

            // Clear strategy to force re-planning
            if let Some(session) = state.current_session_mut() {
                session.proof.strategy_summary = None;
                session.proof.phase = "planning".to_string();
                session.proof.status_line = format!(
                    "Backtracked after {} failed repairs. Re-planning.",
                    basis.attempt_count
                );
            }

            // Build context for the new prover: what failed, research hints
            let mut backtrack_context = format!(
                "The previous proof approach for {} FAILED after {} repair attempts. \
                 Do NOT continue repairing it. Try a COMPLETELY DIFFERENT proof strategy.\n\n\
                 Failed approach summary:\n{}\n\nLast error:\n{}\n\n\
                 RESEARCH TASK: Before writing code, first identify the correct Mathlib lemma names \
                 by using #check and exact? tactics. List the 3-5 most relevant Mathlib theorems for this goal. \
                 Then construct a proof using ONLY verified lemma names.\n\n\
                 Think of an entirely different mathematical approach. \
                 If the previous approach used Chevalley-Warning, try pigeonhole. \
                 If it used induction, try a direct construction. \
                 ALWAYS use exact? or apply? when you are unsure of a lemma name -- \
                 Lean will search Mathlib for you.",
                target, basis.attempt_count,
                basis.summary,
                basis.last_lean_diagnostic.lines().take(5).collect::<Vec<_>>().join("\n"),
            );

            // Include Lean grounding facts even in backtrack context
            let grounding = openproof_lean::extract_grounding_from_lean_output(
                &basis.last_lean_diagnostic,
                &basis.diagnostics,
            );
            if !grounding.is_empty() {
                backtrack_context.push_str(
                    "\n\nLean DID find these correct facts -- use them even in the new approach:\n",
                );
                for fact in &grounding {
                    backtrack_context.push_str(&format!("  {fact}\n"));
                }
            }

            // Include failed path history
            let target_label = latest_session
                .proof
                .nodes
                .first()
                .map(|n| n.label.as_str())
                .unwrap_or(&latest_session.title);
            if let Ok(failed) = store.failed_attempts_for_target(target_label, 5) {
                if !failed.is_empty() {
                    backtrack_context.push_str(
                        "\n\nAll previously failed approaches (do NOT repeat ANY of these):\n",
                    );
                    for (class, snippet, diag) in &failed {
                        backtrack_context
                            .push_str(&format!("  [{class}] {snippet}\n    -> {diag}\n"));
                    }
                }
            }

            let title = format!("{} alt-prover", latest_session.title);
            let (branch_id, session_snapshot) = ensure_hidden_agent_branch(
                tx.clone(),
                store.clone(),
                state,
                AgentRole::Prover,
                &title,
                "Alternative proof strategy after backtracking",
            )?;
            start_agent_branch_turn(
                tx,
                store,
                AgentRole::Prover,
                backtrack_context,
                branch_id.clone(),
                branch_id.clone(),
                session_snapshot,
            );
            actions.push(format!("Started alternative prover branch {branch_id}."));
            return Ok(actions.join("\n"));
        }

        // Normal repair (< 3 attempts): enrich with grounding + goals + suggestions
        let description = format!(
            "Repair the failing Lean candidate for {} using the latest diagnostics.",
            target
        );
        let title = format!("{} repair", latest_session.title);

        let mut repair_context = String::new();

        // Show current file with line numbers so the model can patch surgically
        let current_content = latest_session
            .proof
            .last_rendered_scratch
            .as_deref()
            .or_else(|| {
                latest_session
                    .proof
                    .nodes
                    .first()
                    .map(|n| n.content.as_str())
            })
            .unwrap_or("");
        if !current_content.trim().is_empty() {
            repair_context.push_str("Current Scratch.lean:\n```\n");
            for (i, line) in current_content.lines().enumerate() {
                repair_context.push_str(&format!("{:4}: {}\n", i + 1, line));
            }
            repair_context.push_str("```\n\n");
            repair_context.push_str(
                "Output a PATCH to fix the errors. Use this format:\n\
                 *** Begin Patch\n\
                 *** Update File: Scratch.lean\n\
                 @@ context line\n\
                  context line (unchanged, prefixed with space)\n\
                 -old broken line (prefixed with -)\n\
                 +fixed line (prefixed with +)\n\
                  context line\n\
                 *** End Patch\n\n\
                 Only change what's needed. Do NOT rewrite the entire file.\n\
                 If you must rewrite, use a ```lean code block instead.\n\n",
            );
        }

        // Grounding facts from Lean output
        let grounding = openproof_lean::extract_grounding_from_lean_output(
            &basis.last_lean_diagnostic,
            &basis.diagnostics,
        );
        if !grounding.is_empty() {
            repair_context.push_str("CRITICAL -- Lean itself found these. USE THEM:\n");
            for fact in &grounding {
                repair_context.push_str(&format!("  {fact}\n"));
            }
            repair_context.push('\n');
        }

        repair_context.push_str(&format!(
            "{description}\n\nLatest diagnostics:\n{}",
            basis.last_lean_diagnostic
        ));

        // Extract sorry goal states
        if !basis.lean_snippet.trim().is_empty() {
            let project_dir = crate::helpers::resolve_lean_project_dir();
            let rendered = openproof_lean::render_node_scratch(
                &latest_session,
                latest_session
                    .proof
                    .nodes
                    .first()
                    .unwrap_or(&openproof_protocol::ProofNode::default()),
            );
            if let Ok(goals) = openproof_lean::extract_sorry_goals(&project_dir, &rendered) {
                if !goals.is_empty() {
                    repair_context.push_str("\n\nUnsolved goals at sorry points:\n");
                    for (line, goal) in &goals {
                        repair_context.push_str(&format!("  Line {line}: {goal}\n"));
                    }

                    // Premise retrieval: search corpus for lemmas matching goal types
                    // This is done synchronously via FTS for now; vector search is async
                    let goal_query = goals
                        .iter()
                        .map(|(_, g)| g.as_str())
                        .collect::<Vec<_>>()
                        .join(" ");
                    if let Ok(premises) = store.search_verified_corpus(&goal_query, 5) {
                        if !premises.is_empty() {
                            repair_context
                                .push_str("\n\nRelevant verified premises from corpus:\n");
                            for (label, statement, _vis) in &premises {
                                repair_context.push_str(&format!("  {label} :: {statement}\n"));
                            }
                        }
                    }
                }
            }

            if basis.attempt_count >= 2 {
                if let Ok(suggestions) =
                    openproof_lean::run_tactic_suggestions(&project_dir, &rendered, "exact?")
                {
                    if !suggestions.is_empty() {
                        repair_context.push_str("\n\nLean's own suggestions (via exact?):\n");
                        for s in suggestions.iter().take(5) {
                            repair_context.push_str(&format!("  {s}\n"));
                        }
                    }
                }
            }
        }

        // Failed path history
        let target_label = latest_session
            .proof
            .nodes
            .first()
            .map(|n| n.label.as_str())
            .unwrap_or(&latest_session.title);
        if let Ok(failed) = store.failed_attempts_for_target(target_label, 5) {
            if !failed.is_empty() {
                repair_context.push_str("\n\nPrevious failed approaches (do NOT repeat these):\n");
                for (class, snippet, diag) in &failed {
                    repair_context.push_str(&format!("  [{class}] {snippet}\n    -> {diag}\n"));
                }
            }
        }

        // Spawn tactic search in parallel with the agentic repair (if strategy allows).
        let strategy = latest_session.proof.search_strategy;
        if matches!(
            strategy,
            SearchStrategy::Hybrid | SearchStrategy::TacticSearch
        ) {
            spawn_tactic_search_for_sorrys(tx.clone(), &latest_session, &store);
            if matches!(strategy, SearchStrategy::TacticSearch) {
                // Pure tactic search mode: skip the agentic branch entirely.
                actions.push("Started tactic search (pure mode, no agentic branch).".to_string());
                return Ok(actions.join("\n"));
            }
        }

        let (branch_id, session_snapshot) = ensure_hidden_agent_branch(
            tx.clone(),
            store.clone(),
            state,
            AgentRole::Repairer,
            &title,
            &description,
        )?;
        start_agent_branch_turn(
            tx,
            store,
            AgentRole::Repairer,
            repair_context,
            branch_id.clone(),
            branch_id.clone(),
            session_snapshot,
        );
        actions.push(format!("Started repairer branch {branch_id}."));
        return Ok(actions.join("\n"));
    }

    // Build rich context for Prover/Planner branches (same pattern as Repairer)
    let branch_context = {
        let mut ctx = String::new();
        // Show workspace files
        if let Ok(files) = store.list_workspace_files(&latest_session.id) {
            let lean_files: Vec<_> = files
                .iter()
                .filter(|(p, _)| p.ends_with(".lean") && !p.contains("history/"))
                .collect();
            if !lean_files.is_empty() {
                ctx.push_str("Workspace files:\n");
                for (path, size) in &lean_files {
                    ctx.push_str(&format!("  {path} ({size} bytes)\n"));
                }
                ctx.push('\n');
            }
        }
        // Show primary file content with line numbers
        let ws_dir = store.workspace_dir(&latest_session.id);
        for name in &["Main.lean", "Scratch.lean", "Helpers.lean", "Defs.lean"] {
            let path = ws_dir.join(name);
            if let Ok(content) = std::fs::read_to_string(&path) {
                if !content.trim().is_empty() && content.lines().count() <= 200 {
                    ctx.push_str(&format!("{name}:\n```lean\n"));
                    for (i, line) in content.lines().enumerate() {
                        ctx.push_str(&format!("{:4}: {}\n", i + 1, line));
                    }
                    ctx.push_str("```\n\n");
                }
            }
        }
        // Strategy summary if available
        if let Some(summary) = latest_session
            .proof
            .strategy_summary
            .as_ref()
            .filter(|s| !s.trim().is_empty())
        {
            ctx.push_str(&format!("Strategy: {summary}\n\n"));
        }
        // Include past failed attempts so branches don't repeat them
        let target_label = latest_session
            .proof
            .nodes
            .first()
            .map(|n| n.label.as_str())
            .unwrap_or(&latest_session.title);
        if let Ok(failed) = store.failed_attempts_for_target(target_label, 5) {
            if !failed.is_empty() {
                ctx.push_str("PREVIOUSLY FAILED APPROACHES (do NOT repeat these):\n");
                for (class, snippet, diag) in &failed {
                    ctx.push_str(&format!("  [{class}] {snippet} -> {diag}\n"));
                }
                ctx.push('\n');
            }
        }
        // Related corpus items
        if let Some(active) = latest_session
            .proof
            .active_node_id
            .as_deref()
            .and_then(|id| latest_session.proof.nodes.iter().find(|n| n.id == id))
        {
            let item_key = format!("user-verified/{}/{}", latest_session.id, active.label);
            if let Ok(related) = store.get_related_items(&item_key, 5) {
                if !related.is_empty() {
                    ctx.push_str("RELATED ITEMS (from corpus):\n");
                    for (to_key, edge_type, confidence) in &related {
                        let label = to_key.rsplit('/').next().unwrap_or(to_key);
                        ctx.push_str(&format!("  [{edge_type} conf={confidence:.1}] {label}\n"));
                    }
                    ctx.push('\n');
                }
            }
        }
        // Explicit tool instructions
        ctx.push_str(
            "INSTRUCTIONS: Use tools to work on this code.\n\
             1. file_read to see current files with line numbers\n\
             2. file_patch to modify code (fill sorrys, fix errors)\n\
             3. lean_verify to check your changes compile\n\
             Do NOT output code as text. Use file_patch tool.\n\n",
        );
        ctx
    };

    if latest_session
        .proof
        .strategy_summary
        .as_ref()
        .map(|item| item.trim().is_empty())
        .unwrap_or(true)
    {
        let description = format!(
            "{branch_context}Write an INFORMAL PROOF SKETCH for: {target}\n\n\
             Research the proof technique. Identify the key mathematical insight. \
             Decompose into lemmas. Write the sketch as comments in the Lean file using file_patch."
        );
        let title = format!("{} planner", latest_session.title);
        let (branch_id, session_snapshot) = ensure_hidden_agent_branch(
            tx.clone(),
            store.clone(),
            state,
            AgentRole::Planner,
            &title,
            &description,
        )?;
        start_agent_branch_turn(
            tx.clone(),
            store.clone(),
            AgentRole::Planner,
            description,
            branch_id.clone(),
            branch_id.clone(),
            session_snapshot,
        );
        actions.push(format!("Started planner branch {branch_id}."));
    }

    let latest_session = state
        .current_session()
        .cloned()
        .ok_or_else(|| "No active session.".to_string())?;
    let has_foreground = current_foreground_branch(Some(&latest_session)).is_some();
    if has_foreground {
        let description =
            format!("{branch_context}Produce an alternate Lean proof candidate for {target}.");
        let title = format!("{} search prover", latest_session.title);
        let (branch_id, session_snapshot) = ensure_hidden_agent_branch(
            tx.clone(),
            store.clone(),
            state,
            AgentRole::Prover,
            &title,
            &description,
        )?;
        start_agent_branch_turn(
            tx,
            store,
            AgentRole::Prover,
            description,
            branch_id.clone(),
            branch_id.clone(),
            session_snapshot,
        );
        actions.push(format!("Started hidden prover branch {branch_id}."));
    } else {
        let title = format!("{} prover", latest_session.title);
        let description = format!("{branch_context}Produce a Lean proof candidate for {target}.");
        let (write, branch_id, task_id) =
            state.spawn_agent_branch(AgentRole::Prover, &title, &description, false)?;
        let session_snapshot = write.session.clone();
        persist_write(tx.clone(), store.clone(), write);
        start_agent_branch_turn(
            tx,
            store,
            AgentRole::Prover,
            description,
            branch_id.clone(),
            task_id,
            session_snapshot,
        );
        actions.push(format!("Started foreground prover branch {branch_id}."));
    }

    if actions.is_empty() {
        Ok("Autonomous loop found no new branch to schedule.".to_string())
    } else {
        Ok(actions.join("\n"))
    }
}

fn refresh_retrieval_branch(
    tx: mpsc::UnboundedSender<AppEvent>,
    store: AppStore,
    state: &mut AppState,
) -> Result<String, String> {
    let Some(session) = state.current_session().cloned() else {
        return Err("No active session.".to_string());
    };
    let query = session
        .proof
        .active_node_id
        .as_deref()
        .and_then(|node_id| session.proof.nodes.iter().find(|node| node.id == node_id))
        .map(|node| node.statement.clone())
        .or_else(|| session.proof.accepted_target.clone())
        .or_else(|| session.proof.formal_target.clone())
        .unwrap_or_default();
    if query.trim().is_empty() {
        return Ok("No target is ready for verified retrieval yet.".to_string());
    }
    let hits = store
        .search_verified_corpus(&query, 6)
        .map_err(|error| error.to_string())?;
    let summary = if hits.is_empty() {
        "No strong verified references found for the current target.".to_string()
    } else {
        format!(
            "Retrieved {} verified references. Best hit: {}.",
            hits.len(),
            hits.first()
                .map(|item| item.0.clone())
                .unwrap_or_else(|| "n/a".to_string())
        )
    };

    let branch_id = state.current_session().and_then(|current| {
        current
            .proof
            .branches
            .iter()
            .filter(|branch| branch.hidden && branch.role == AgentRole::Retriever)
            .max_by(|left, right| left.updated_at.cmp(&right.updated_at))
            .map(|branch| branch.id.clone())
    });
    let branch_id = if let Some(branch_id) = branch_id {
        let now = chrono::Utc::now().to_rfc3339();
        if let Some(current) = state.current_session_mut() {
            if let Some(branch) = current
                .proof
                .branches
                .iter_mut()
                .find(|branch| branch.id == branch_id)
            {
                branch.hidden = true;
                branch.branch_kind = "retriever_hidden".to_string();
                branch.status = AgentStatus::Done;
                branch.queue_state = BranchQueueState::Done;
                branch.phase = Some("retrieving".to_string());
                branch.goal_summary = query.clone();
                branch.score = if hits.is_empty() {
                    0.0
                } else {
                    18.0 + hits.len() as f32 * 3.0
                };
                branch.progress_kind = Some("retrieving".to_string());
                branch.search_status = summary.clone();
                branch.summary = hits
                    .iter()
                    .take(3)
                    .map(|(label, statement, visibility)| {
                        format!("{label} [{visibility}] :: {statement}")
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                branch.updated_at = now.clone();
            }
            current.updated_at = now;
            current.proof.active_retrieval_summary = Some(summary.clone());
        }
        persist_current_session(
            tx.clone(),
            store.clone(),
            state,
            "Updated verified retrieval branch.".to_string(),
        );
        branch_id
    } else {
        let (branch_id, snapshot) = ensure_hidden_agent_branch(
            tx.clone(),
            store.clone(),
            state,
            AgentRole::Retriever,
            "Verified retrieval",
            &query,
        )?;
        if let Some(current) = state.current_session_mut() {
            if let Some(branch) = current
                .proof
                .branches
                .iter_mut()
                .find(|branch| branch.id == branch_id)
            {
                branch.status = AgentStatus::Done;
                branch.queue_state = BranchQueueState::Done;
                branch.phase = Some("retrieving".to_string());
                branch.score = if hits.is_empty() {
                    0.0
                } else {
                    18.0 + hits.len() as f32 * 3.0
                };
                branch.progress_kind = Some("retrieving".to_string());
                branch.search_status = summary.clone();
                branch.summary = hits
                    .iter()
                    .take(3)
                    .map(|(label, statement, visibility)| {
                        format!("{label} [{visibility}] :: {statement}")
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
            }
            current.proof.active_retrieval_summary = Some(summary.clone());
        }
        let _ = snapshot;
        persist_current_session(
            tx.clone(),
            store.clone(),
            state,
            "Recorded verified retrieval hits.".to_string(),
        );
        branch_id
    };

    if let Ok(write) = state.refresh_hidden_search_state(Some(Some(summary.clone()))) {
        persist_write(tx, store, write);
    }
    Ok(format!("{} [{}]", summary, branch_id))
}

/// Consume events from `rx` until all active branches have finished and there
/// are no in-flight turns or verifications, or until a 5-minute deadline.
///
/// Branch finish events trigger a verification spawn if the branch produced a
/// Lean snippet.
pub async fn drain_until_settled(
    tx: mpsc::UnboundedSender<AppEvent>,
    store: AppStore,
    state: &mut AppState,
    rx: &mut mpsc::UnboundedReceiver<AppEvent>,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
    loop {
        match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
            Ok(Some(event)) => {
                let mut finished_branch_id: Option<String> = None;
                match &event {
                    AppEvent::AppendBranchAssistant {
                        branch_id,
                        content,
                        used_tools,
                    } => {
                        let lean_count = content.matches("```lean").count();
                        eprintln!(
                            "[run] Branch {branch_id}: {len} chars, {lean_count} lean block(s), tools={used_tools}",
                            len = content.len()
                        );
                    }
                    AppEvent::FinishBranch {
                        branch_id,
                        status,
                        summary,
                        ..
                    } => {
                        eprintln!("[run] Branch {branch_id} finished: {status:?} -- {summary}");
                        finished_branch_id = Some(branch_id.clone());
                    }
                    AppEvent::LeanVerifyStarted => {
                        eprintln!("[run] Lean verification started...");
                    }
                    AppEvent::LeanVerifyFinished(r) => {
                        eprintln!("[run] Verify: ok={}, code={:?}", r.ok, r.code);
                        if !r.ok {
                            for l in r.stderr.lines().take(3) {
                                eprintln!("[run]   {l}");
                            }
                        }
                    }
                    AppEvent::BranchVerifyFinished {
                        branch_id,
                        result,
                        promote,
                        ..
                    } => {
                        if result.ok {
                            eprintln!(
                                "[run] *** BRANCH {branch_id} VERIFIED (promote={promote}) ***"
                            );
                        } else {
                            eprintln!("[run] Branch {branch_id} verify failed");
                            for l in result.stderr.lines().take(3) {
                                eprintln!("[run]   {l}");
                            }
                        }
                    }
                    AppEvent::AppendNotice { title, content } => {
                        eprintln!(
                            "[run] {title}: {}",
                            content.chars().take(200).collect::<String>()
                        );
                    }
                    AppEvent::PersistSucceeded(_) | AppEvent::PersistFailed(_) => {}
                    _ => {}
                }
                if let Some(write) = state.apply(event) {
                    persist_write(tx.clone(), store.clone(), write);
                }

                if let Some(bid) = finished_branch_id {
                    if let Some(session_snapshot) = state.current_session().cloned() {
                        let branch_info = session_snapshot
                            .proof
                            .branches
                            .iter()
                            .find(|b| b.id == bid)
                            .map(|b| (b.lean_snippet.trim().is_empty(), b.hidden));
                        if let Some((snippet_empty, hidden)) = branch_info {
                            if !snippet_empty {
                                eprintln!(
                                    "[run] Branch {} has lean snippet, starting verification...",
                                    bid
                                );
                                start_branch_verification(
                                    tx.clone(),
                                    store.clone(),
                                    session_snapshot,
                                    bid.clone(),
                                    !hidden,
                                );
                            } else {
                                eprintln!("[run] Branch {} finished with no lean candidate.", bid);
                            }
                        }
                    }
                }

                let s = state.current_session().cloned().unwrap();
                let all_done = s
                    .proof
                    .branches
                    .iter()
                    .all(|b| !matches!(b.status, AgentStatus::Running));
                if all_done && !state.turn_in_flight && !state.verification_in_flight {
                    return;
                }
            }
            Ok(None) => return,
            Err(_) => {
                let s = state.current_session().cloned().unwrap();
                let running = s
                    .proof
                    .branches
                    .iter()
                    .filter(|b| b.status == AgentStatus::Running)
                    .count();
                if running == 0 && !state.turn_in_flight && !state.verification_in_flight {
                    return;
                }
                if tokio::time::Instant::now() > deadline {
                    eprintln!("[run] Timeout waiting for tasks.");
                    return;
                }
            }
        }
    }
}

pub async fn run_tactic_search_once(session_id: String) -> Result<()> {
    let store = AppStore::open(StorePaths::detect()?)?;
    let session = store
        .get_session(&session_id)?
        .with_context(|| format!("missing session {session_id}"))?;

    let workspace_dir = store.workspace_dir(&session.id);
    let mut full_content = String::new();
    if let Ok(files) = store.list_workspace_files(&session.id) {
        for (path, _) in &files {
            if path.ends_with(".lean") && !path.contains("history/") {
                if let Ok(content) = fs::read_to_string(workspace_dir.join(path)) {
                    if !full_content.is_empty() {
                        full_content.push_str("\n\n");
                    }
                    full_content.push_str(&content);
                }
            }
        }
    }
    if full_content.trim().is_empty() {
        full_content = session
            .proof
            .nodes
            .first()
            .map(|node| node.content.clone())
            .unwrap_or_default();
    }

    let sorry_count = openproof_lean::find_sorry_positions(&full_content).len();
    anyhow::ensure!(
        sorry_count > 0,
        "Session {session_id} has no sorrys in workspace or node content"
    );

    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
    eprintln!(
        "[tactic-search] Running search for session {} with {} sorry goal(s)",
        session.id, sorry_count
    );
    spawn_tactic_search_for_sorrys(tx, &session, &store);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(240);
    let mut completed = 0usize;
    while completed < sorry_count {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        anyhow::ensure!(
            !remaining.is_zero(),
            "Timed out waiting for tactic search results for session {session_id}"
        );
        match tokio::time::timeout(remaining.min(Duration::from_secs(30)), rx.recv()).await {
            Ok(Some(AppEvent::TacticSearchComplete {
                sorry_line,
                solved,
                tactics,
                remaining_goals,
                search_outcome,
                ..
            })) => {
                completed += 1;
                eprintln!(
                    "[tactic-search] line {sorry_line}: {search_outcome} (solved={solved}, remaining={:?}, tactics={})",
                    remaining_goals,
                    tactics.join("; ")
                );
            }
            Ok(Some(AppEvent::ProofGoalUpdated(goal))) => {
                if !goal.goal_text.trim().is_empty() {
                    eprintln!(
                        "[tactic-search] goal update: {}",
                        goal.goal_text.lines().next().unwrap_or("")
                    );
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {}
        }
    }

    anyhow::ensure!(
        completed == sorry_count,
        "Expected {sorry_count} tactic search result(s) for session {session_id}, received {completed}"
    );
    Ok(())
}

/// Run tactic search on a standalone Lean file (no session needed).
/// Creates a temporary workspace and session, runs search, exports verified data.
pub async fn run_tactic_search_file(file_path: String) -> Result<()> {
    let content = fs::read_to_string(&file_path).with_context(|| format!("reading {file_path}"))?;

    let sorry_count = openproof_lean::find_sorry_positions(&content).len();
    anyhow::ensure!(sorry_count > 0, "No sorrys found in {file_path}");

    let store = AppStore::open(StorePaths::detect()?)?;

    // Create a minimal session via the store's own API
    let session_id = format!("expert_file_{}", chrono::Utc::now().timestamp_millis());

    // Write the lean file to a workspace directory the store can find
    let workspace_dir = store.workspace_dir(&session_id);
    fs::create_dir_all(&workspace_dir)?;
    fs::write(workspace_dir.join("Main.lean"), &content)?;

    // Extract the type expression for the fallback goal
    let type_expr = extract_type_expr_from_content(&content).unwrap_or_default();

    let mut node = openproof_protocol::ProofNode::default();
    node.id = "root".to_string();
    node.content = content.clone();
    node.statement = type_expr;
    node.status = openproof_protocol::ProofNodeStatus::Pending;
    let mut proof = openproof_protocol::ProofSessionState::default();
    proof.nodes.push(node);
    proof.active_node_id = Some("root".to_string());

    let session = openproof_protocol::SessionSnapshot {
        id: session_id.clone(),
        title: file_path.clone(),
        proof,
        ..Default::default()
    };
    store.save_session(&session)?;

    eprintln!("[tactic-search] File: {file_path}, {sorry_count} sorry(s), session: {session_id}");

    run_tactic_search_once(session_id).await
}

fn tactic_proposer_backend(config: Option<&crate::setup::SetupResult>) -> TacticProposerBackend {
    if let Ok(value) = std::env::var("OPENPROOF_TACTIC_PROPOSER") {
        let normalized = value.trim().to_ascii_lowercase();
        return match normalized.as_str() {
            "codex" => TacticProposerBackend::Codex,
            "ollama" => TacticProposerBackend::Ollama,
            "standard" | "fallback" | "none" => TacticProposerBackend::Standard,
            _ => {
                eprintln!(
                    "[tactic-search] Unknown OPENPROOF_TACTIC_PROPOSER={value:?}; falling back to config"
                );
                tactic_proposer_backend_from_config(config)
            }
        };
    }

    tactic_proposer_backend_from_config(config)
}

fn tactic_proposer_backend_from_config(
    config: Option<&crate::setup::SetupResult>,
) -> TacticProposerBackend {
    match config {
        Some(cfg) if cfg.model_provider == "codex" => TacticProposerBackend::Codex,
        Some(cfg) if cfg.prover_model.is_some() => TacticProposerBackend::Ollama,
        _ => TacticProposerBackend::Standard,
    }
}

fn codex_tactic_model() -> String {
    std::env::var("OPENPROOF_TACTIC_MODEL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_CODEX_TACTIC_MODEL.to_string())
}

fn build_retrieval_context(hits: &[(String, String, String)]) -> String {
    hits.iter()
        .take(6)
        .map(|(label, statement, _visibility)| format!("{label} : {statement}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn build_codex_tactic_prompt(goal: &str, retrieval_context: &str, max_candidates: usize) -> String {
    let mut prompt = format!(
        "Current Lean goal:\n{goal}\n\nReturn a compact JSON object of the form {{\"tactics\":[\"...\"]}} with up to {max_candidates} single-line Lean tactics."
    );
    if !retrieval_context.trim().is_empty() {
        prompt.push_str("\n\nRelevant verified declarations:\n");
        prompt.push_str(retrieval_context);
    }
    prompt
}

fn propose_codex_tactics(
    model: &str,
    goal: &str,
    retrieval_context: &str,
    max_candidates: usize,
) -> Result<Vec<String>> {
    if goal.trim().is_empty() || max_candidates == 0 {
        return Ok(Vec::new());
    }

    let prompt = build_codex_tactic_prompt(goal, retrieval_context, max_candidates);
    let messages = vec![
        TurnMessage::chat(
            "system",
            "You are proposing Lean 4 tactics for OpenProof. Reply with strict JSON only. The response must be an object with a single key named tactics whose value is an array of single-line tactic strings. No markdown, no explanation, no code fences, and never emit sorry, admit, or native_decide.",
        ),
        TurnMessage::chat("user", prompt),
    ];
    let session_id = format!("tactic-search-{}", chrono::Utc::now().timestamp_millis());
    let request = CodexTurnRequest {
        session_id: &session_id,
        messages: &messages,
        model,
        reasoning_effort: DEFAULT_CODEX_REASONING_EFFORT,
        include_tools: false,
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("building Codex tactic runtime")?;
    let response = runtime
        .block_on(async {
            tokio::time::timeout(
                Duration::from_secs(30),
                openproof_model::run_codex_turn(request),
            )
            .await
        })
        .context("Codex tactic proposal timed out")??;

    Ok(parse_codex_tactics(&response)
        .into_iter()
        .filter_map(|tactic| filter_candidate_tactic(&tactic))
        .take(max_candidates)
        .collect())
}

fn parse_codex_tactics(response: &str) -> Vec<String> {
    if let Some(parsed) = parse_codex_tactic_json(response) {
        return parsed.tactics;
    }

    let trimmed = response.trim();
    if let Some(stripped) = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
    {
        let inner = stripped.trim().trim_end_matches("```").trim();
        if let Some(parsed) = parse_codex_tactic_json(inner) {
            return parsed.tactics;
        }
    }

    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}')) {
        if start < end {
            if let Some(parsed) = parse_codex_tactic_json(&trimmed[start..=end]) {
                return parsed.tactics;
            }
        }
    }

    trimmed
        .lines()
        .map(|line| {
            line.trim()
                .trim_start_matches('-')
                .trim_start_matches('*')
                .trim()
                .trim_matches('"')
                .to_string()
        })
        .filter(|line| !line.is_empty())
        .collect()
}

fn parse_codex_tactic_json(response: &str) -> Option<CodexTacticResponse> {
    serde_json::from_str::<CodexTacticResponse>(response).ok()
}

fn filter_candidate_tactic(raw: &str) -> Option<String> {
    let tactic = raw
        .lines()
        .next()
        .unwrap_or(raw)
        .trim()
        .trim_end_matches(":::")
        .trim()
        .to_string();

    if tactic.is_empty() {
        return None;
    }

    let lower = tactic.to_ascii_lowercase();
    for banned in ["sorry", "admit", "native_decide"] {
        if lower == banned
            || lower.starts_with(&format!("{banned} "))
            || lower.starts_with(&format!("{banned};"))
        {
            return None;
        }
    }

    if tactic.contains("?_") {
        return None;
    }

    Some(tactic)
}

fn format_tactic_training_prompt(goal_state: &str) -> String {
    format!("{goal_state}:::")
}

fn expert_data_dir() -> Result<PathBuf> {
    let base_dirs = BaseDirs::new().context("could not resolve home directory")?;
    Ok(base_dirs.home_dir().join(".openproof").join("expert-data"))
}

fn expert_data_write_lock() -> &'static Mutex<()> {
    EXPERT_DATA_WRITE_LOCK.get_or_init(|| Mutex::new(()))
}

fn append_jsonl_record<T: Serialize>(path: &Path, record: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }

    let _guard = expert_data_write_lock()
        .lock()
        .map_err(|e| anyhow::anyhow!("expert-data lock poisoned: {e}"))?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    let line = serde_json::to_string(record)?;
    writeln!(file, "{line}").with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn export_verified_tactic_examples(
    pantograph: &Arc<Mutex<openproof_lean::pantograph::Pantograph>>,
    export: &ExpertExportContext,
    sorry_line: usize,
    root_goal: &str,
    tactics: &[String],
) -> Result<usize> {
    if root_goal.trim().is_empty() || tactics.is_empty() {
        return Ok(0);
    }

    let export_dir = expert_data_dir()?;
    let positives_path = export_dir.join("positives.jsonl");
    let trajectories_path = export_dir.join("trajectories.jsonl");
    let created_at = chrono::Utc::now().to_rfc3339();

    let mut states_to_delete = Vec::new();
    let (positive_records, goals_before) = {
        let mut pg = pantograph
            .lock()
            .map_err(|e| anyhow::anyhow!("pantograph lock poisoned: {e}"))?;
        if !pg.is_alive() {
            anyhow::bail!("Pantograph process is not running");
        }

        let replay_result = (|| -> Result<(Vec<ExpertPositiveRecord>, Vec<String>)> {
            let initial_state = pg
                .start_goal(root_goal)?
                .with_context(|| format!("goal.start failed for {root_goal}"))?;
            states_to_delete.push(initial_state);

            let mut current_state = initial_state;
            let mut current_goal = root_goal.to_string();
            let mut goals_before = Vec::with_capacity(tactics.len());
            let mut positive_records = Vec::with_capacity(tactics.len());
            for (step_index, tactic) in tactics.iter().enumerate() {
                goals_before.push(current_goal.clone());
                positive_records.push(ExpertPositiveRecord {
                    prompt: format_tactic_training_prompt(&current_goal),
                    completion: tactic.clone(),
                    goal_state: current_goal.clone(),
                    step_index,
                    session_id: export.session_id.clone(),
                    node_id: export.node_id.clone(),
                    node_label: export.node_label.clone(),
                    node_statement: export.node_statement.clone(),
                    sorry_line,
                    backend: export.backend.clone(),
                    model: export.model.clone(),
                    created_at: created_at.clone(),
                });

                let result = pg.try_tactic(current_state, 0, tactic)?;
                if result.error.is_some() || result.new_state_id.is_none() {
                    anyhow::bail!("replay failed for tactic {tactic:?}: {:?}", result.error);
                }

                current_state = result.new_state_id.unwrap();
                states_to_delete.push(current_state);

                if step_index + 1 == tactics.len() {
                    if !result.remaining_goals.is_empty() {
                        anyhow::bail!(
                            "final tactic {tactic:?} did not close the goal; {} goals remain",
                            result.remaining_goals.len()
                        );
                    }
                } else {
                    current_goal = result
                        .remaining_goals
                        .first()
                        .cloned()
                        .context("missing next goal while replaying solved trajectory")?;
                }
            }
            Ok((positive_records, goals_before))
        })();

        for state_id in states_to_delete.iter().rev() {
            let _ = pg.delete_goal(*state_id);
        }

        replay_result?
    };

    for record in &positive_records {
        append_jsonl_record(&positives_path, record)?;
    }

    append_jsonl_record(
        &trajectories_path,
        &ExpertTrajectoryRecord {
            root_goal: root_goal.to_string(),
            goals_before,
            tactics: tactics.to_vec(),
            session_id: export.session_id.clone(),
            node_id: export.node_id.clone(),
            node_label: export.node_label.clone(),
            node_statement: export.node_statement.clone(),
            sorry_line,
            backend: export.backend.clone(),
            model: export.model.clone(),
            created_at,
        },
    )?;

    Ok(tactics.len())
}

/// Spawn best-first tactic search tasks for each sorry position in the active
/// node's content. Each sorry gets its own search task, running in parallel
/// with any agentic branches. Results come back as `TacticSearchComplete` events.
fn spawn_tactic_search_for_sorrys(
    tx: mpsc::UnboundedSender<AppEvent>,
    session: &openproof_protocol::SessionSnapshot,
    store: &AppStore,
) {
    let node_id = session.proof.active_node_id.clone().unwrap_or_default();
    let active_node = session.proof.nodes.iter().find(|node| node.id == node_id);
    let node_label = active_node
        .map(|node| node.label.clone())
        .unwrap_or_default();
    let node_statement = active_node
        .map(|node| node.statement.clone())
        .unwrap_or_default();
    let fallback_goal_type = if !node_statement.trim().is_empty() {
        node_statement.clone()
    } else {
        session
            .proof
            .accepted_target
            .clone()
            .or_else(|| session.proof.formal_target.clone())
            .unwrap_or_default()
    };
    let project_dir = crate::helpers::resolve_lean_project_dir();
    let workspace_dir = store.workspace_dir(&session.id);

    // Read ALL workspace .lean files and concatenate for sorry analysis.
    // The workspace files (Main.lean, Helpers.lean) are the source of truth.
    let mut full_content = String::new();
    if let Ok(files) = store.list_workspace_files(&session.id) {
        for (path, _) in &files {
            if path.ends_with(".lean") && !path.contains("history/") {
                if let Ok(content) = std::fs::read_to_string(workspace_dir.join(path)) {
                    if !full_content.is_empty() {
                        full_content.push_str("\n\n");
                    }
                    full_content.push_str(&content);
                }
            }
        }
    }
    // Fallback to node.content
    if full_content.trim().is_empty() {
        full_content = session
            .proof
            .nodes
            .first()
            .map(|n| n.content.clone())
            .unwrap_or_default();
    }
    if full_content.trim().is_empty() {
        return;
    }

    let sorry_positions = openproof_lean::find_sorry_positions(&full_content);
    if sorry_positions.is_empty() {
        return;
    }

    // Write the full content to the Lean project dir for the LSP to read.
    // The MCP requires files to be inside a Lean project (with lean-toolchain).
    let scratch_path = project_dir.join("Scratch.lean");
    let _ = std::fs::write(&scratch_path, &full_content);

    // Standard tactics for the propose_fn callback (used as fallback)
    let standard_tactics: Vec<String> = vec![
        "simp",
        "omega",
        "ring",
        "norm_num",
        "linarith",
        "aesop",
        "grind",
        "decide",
        "trivial",
        "exact?",
        "apply?",
        "simp_all",
        "tauto",
        "contradiction",
        "norm_cast",
        "positivity",
        "gcongr",
        "polyrith",
        "field_simp",
        "push_cast",
        "ring_nf",
        "nlinarith",
        "norm_num [*]",
        "simp [*]",
        "grind?",
    ]
    .into_iter()
    .map(String::from)
    .collect();

    let setup_config = crate::setup::load_config();
    let proposer_backend = tactic_proposer_backend(setup_config.as_ref());
    let codex_model = if proposer_backend == TacticProposerBackend::Codex {
        let _ = openproof_model::sync_auth_from_codex_cli();
        Some(codex_tactic_model())
    } else {
        None
    };
    let codex_cache: Arc<Mutex<HashMap<String, Vec<String>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    // Check if a prover model is configured and available via ollama
    let ollama_proposer = if proposer_backend == TacticProposerBackend::Ollama {
        let proposer = if let Some(model_tag) = setup_config
            .as_ref()
            .and_then(|config| config.prover_model.clone())
        {
            openproof_search::ollama::OllamaProposer::with_model(
                &model_tag,
                "http://localhost:11434",
            )
        } else {
            openproof_search::ollama::OllamaProposer::new()
        };
        if proposer.is_available() {
            eprintln!("[tactic-search] Using ollama tactic proposer");
            Some(Arc::new(proposer))
        } else {
            eprintln!("[tactic-search] Ollama unavailable, using fallback tactics");
            None
        }
    } else {
        None
    };

    if proposer_backend == TacticProposerBackend::Codex {
        eprintln!(
            "[tactic-search] Using Codex tactic proposer ({})",
            codex_model.as_deref().unwrap_or(DEFAULT_CODEX_TACTIC_MODEL)
        );
    } else if proposer_backend == TacticProposerBackend::Standard {
        eprintln!("[tactic-search] Using standard fallback tactics only");
    }

    // Try Pantograph first (1000x faster), fall back to LSP
    eprintln!(
        "[tactic-search] Lean project dir: {}",
        project_dir.display()
    );
    let pantograph: Option<Arc<Mutex<openproof_lean::pantograph::Pantograph>>> =
        match openproof_lean::pantograph::Pantograph::spawn(&project_dir) {
            Ok(pg) => {
                eprintln!("[tactic-search] Pantograph spawned successfully");
                Some(Arc::new(Mutex::new(pg)))
            }
            Err(e) => {
                eprintln!("[tactic-search] Pantograph spawn failed: {e}");
                None
            }
        };

    let lsp_mcp: Option<Arc<Mutex<LeanLspMcp>>> = if pantograph.is_none() {
        LeanLspMcp::spawn(&project_dir)
            .map(|client| Arc::new(Mutex::new(client)))
            .ok()
    } else {
        None // don't spawn LSP if we have Pantograph
    };

    if pantograph.is_none() && lsp_mcp.is_none() {
        eprintln!("[tactic-search] Neither Pantograph nor lean-lsp-mcp available");
        return;
    }

    let config = TacticSearchConfig::default();

    // Extract goal types for each sorry position (one compile, shared across all sorrys)
    let sorry_goals: Vec<(usize, String)> = if pantograph.is_some() {
        // Use lean to extract goal types at each sorry position
        let verify_output = openproof_lean::tools::run_lean_verify_raw(&project_dir, &full_content)
            .ok()
            .map(|(_, output)| output);
        let mut goals = Vec::new();
        for &(line, _col) in &sorry_positions {
            let mut goal = verify_output
                .as_deref()
                .map(|output| extract_goal_at_line(output, line))
                .filter(|goal| !goal.trim().is_empty())
                .unwrap_or_else(|| fallback_goal_type.clone());
            // If goal is still empty, try extracting from the file content directly
            if goal.trim().is_empty() {
                goal = extract_type_expr_from_content(&full_content).unwrap_or_default();
            }
            eprintln!(
                "[tactic-search] sorry line {line}: goal={:?}",
                &goal[..goal.len().min(80)]
            );
            goals.push((line, goal));
        }
        goals
    } else {
        sorry_positions
            .iter()
            .map(|&(line, _)| (line, String::new()))
            .collect()
    };

    for (line, goal_type) in &sorry_goals {
        let line = *line;
        let tx = tx.clone();
        let node_id = node_id.clone();
        let config = config.clone();
        let tactics = standard_tactics.clone();
        let store_for_propose = store.clone();
        let ollama = ollama_proposer.clone();
        let codex_model = codex_model.clone();
        let codex_cache = codex_cache.clone();
        let export = ExpertExportContext {
            session_id: session.id.clone(),
            node_id: node_id.clone(),
            node_label: node_label.clone(),
            node_statement: node_statement.clone(),
            backend: proposer_backend.as_str().to_string(),
            model: codex_model.clone(),
        };
        let propose_fn: openproof_search::search::ProposeFn = Box::new(
            move |goal: &str, _context: &str, k: usize| {
                let mut candidates: Vec<String> = Vec::new();
                let hits = if goal.is_empty() {
                    Vec::new()
                } else {
                    store_for_propose
                        .search_verified_corpus(goal, 8)
                        .unwrap_or_default()
                };
                let retrieval_context = build_retrieval_context(&hits);

                // 1. Model-based tactics
                match proposer_backend {
                    TacticProposerBackend::Codex => {
                        let model_budget = k.min(MAX_CODEX_TACTIC_CANDIDATES);
                        if model_budget > 0 {
                            let cached = codex_cache
                                .lock()
                                .ok()
                                .and_then(|cache| cache.get(goal).cloned());
                            let model_tactics = if let Some(cached) = cached {
                                cached
                            } else if let Some(model) = codex_model.as_deref() {
                                match propose_codex_tactics(
                                    model,
                                    goal,
                                    &retrieval_context,
                                    model_budget,
                                ) {
                                    Ok(model_tactics) => {
                                        if let Ok(mut cache) = codex_cache.lock() {
                                            cache.insert(goal.to_string(), model_tactics.clone());
                                        }
                                        model_tactics
                                    }
                                    Err(error) => {
                                        eprintln!(
                                            "[tactic-search] Codex proposal failed for goal {:?}: {error}",
                                            goal.lines().next().unwrap_or("")
                                        );
                                        Vec::new()
                                    }
                                }
                            } else {
                                Vec::new()
                            };
                            candidates.extend(model_tactics.into_iter().take(model_budget));
                        }
                    }
                    TacticProposerBackend::Ollama => {
                        if let Some(ref proposer) = ollama {
                            if let Ok(model_tactics) = proposer.propose_tactics(goal, k) {
                                candidates.extend(model_tactics);
                            }
                        }
                    }
                    TacticProposerBackend::Standard => {}
                }

                // 2. Corpus-based tactics (premise retrieval)
                if !hits.is_empty() && candidates.len() < k {
                    let mut seen: HashSet<String> = candidates.iter().cloned().collect();
                    for (label, _statement, _vis) in &hits {
                        for tactic in [
                            format!("exact {label}"),
                            format!("apply {label}"),
                            format!("rw [{label}]"),
                        ] {
                            if candidates.len() >= k {
                                break;
                            }
                            if seen.insert(tactic.clone()) {
                                candidates.push(tactic);
                            }
                        }
                        if candidates.len() >= k {
                            break;
                        }
                    }
                }

                // 3. Standard automation tactics as fallback
                let mut seen: HashSet<String> = candidates.iter().cloned().collect();
                for t in &tactics {
                    if candidates.len() >= k {
                        break;
                    }
                    if seen.insert(t.clone()) {
                        candidates.push(t.clone());
                    }
                }

                candidates.truncate(k);
                Ok(candidates)
            },
        );

        // Prefer Pantograph path (3ms per tactic)
        if let Some(ref pg) = pantograph {
            if !goal_type.is_empty() {
                let pg = pg.clone();
                let goal_type = goal_type.clone();
                let export = export.clone();
                tokio::task::spawn_blocking(move || {
                    eprintln!(
                        "[tactic-search] Pantograph search at line {line}: {}",
                        &goal_type[..goal_type.len().min(60)]
                    );
                    let on_goal = {
                        let tx = tx.clone();
                        move |goal: openproof_protocol::ProofGoal| {
                            let _ = tx.send(AppEvent::ProofGoalUpdated(goal));
                        }
                    };
                    match openproof_search::search::pantograph_best_first_search(
                        &pg,
                        &propose_fn,
                        &goal_type,
                        "",
                        &config,
                        Some(&on_goal),
                    ) {
                        Ok(result) => {
                            if let openproof_search::config::SearchResult::Solved {
                                tactics, ..
                            } = &result
                            {
                                match export_verified_tactic_examples(
                                    &pg, &export, line, &goal_type, tactics,
                                ) {
                                    Ok(exported) if exported > 0 => {
                                        eprintln!(
                                            "[tactic-search] Exported {exported} verified tactic examples"
                                        );
                                    }
                                    Ok(_) => {}
                                    Err(error) => {
                                        eprintln!(
                                            "[tactic-search] Verified export failed at line {line}: {error}"
                                        );
                                    }
                                }
                            }
                            emit_search_result(&tx, &node_id, line, result)
                        }
                        Err(e) => eprintln!("[tactic-search] Pantograph error at line {line}: {e}"),
                    }
                });
                continue;
            }
        }

        // Fallback: LSP-based search
        if let Some(ref lsp) = lsp_mcp {
            let lsp = lsp.clone();
            let scratch = scratch_path.clone();
            tokio::task::spawn_blocking(move || {
                eprintln!("[tactic-search] LSP search at line {line}");
                let on_goal = {
                    let tx = tx.clone();
                    move |goal: openproof_protocol::ProofGoal| {
                        let _ = tx.send(AppEvent::ProofGoalUpdated(goal));
                    }
                };
                match best_first_search(
                    &lsp,
                    &propose_fn,
                    &scratch,
                    line,
                    "",
                    &config,
                    Some(&on_goal),
                ) {
                    Ok(result) => emit_search_result(&tx, &node_id, line, result),
                    Err(e) => eprintln!("[tactic-search] LSP error at line {line}: {e}"),
                }
            });
        }
    }
}

fn emit_search_result(
    tx: &mpsc::UnboundedSender<AppEvent>,
    node_id: &str,
    line: usize,
    result: openproof_search::config::SearchResult,
) {
    use openproof_search::config::SearchResult;

    let (solved, tactics, remaining_goals, expansions, outcome) = match &result {
        SearchResult::Solved { tactics, .. } => (true, tactics.clone(), Some(0), None, "solved"),
        SearchResult::Partial {
            tactics,
            remaining_goals,
            ..
        } => (
            false,
            tactics.clone(),
            Some(*remaining_goals),
            None,
            "partial",
        ),
        SearchResult::Exhausted { expansions } => {
            (false, vec![], None, Some(*expansions), "exhausted")
        }
        SearchResult::Timeout {
            best_tactics,
            remaining_goals,
        } => (
            false,
            best_tactics.clone(),
            Some(*remaining_goals),
            None,
            "timeout",
        ),
    };
    eprintln!(
        "[tactic-search] Line {line}: {outcome} (tactics: {}, remaining: {}, expansions: {})",
        tactics.join("; "),
        remaining_goals.map_or("?".to_string(), |g| g.to_string()),
        expansions.map_or("?".to_string(), |e| e.to_string()),
    );
    let _ = tx.send(AppEvent::TacticSearchComplete {
        node_id: node_id.to_string(),
        sorry_line: line,
        solved,
        tactics,
        remaining_goals,
        expansions,
        search_outcome: outcome.to_string(),
    });
}

/// Extract the goal type at a specific sorry line from Lean's error output.
/// Extract a forall type expression from Lean file content containing a theorem with sorry.
/// e.g. "theorem foo (n : Nat) : n + 0 = n := by\n  sorry" -> "forall (n : Nat), n + 0 = n"
fn extract_type_expr_from_content(content: &str) -> Option<String> {
    // Find ":= by" that precedes sorry
    let by_idx = content.rfind(":= by")?;
    let before = &content[..by_idx];

    // Walk backwards to find "theorem", "lemma", or "def"
    let mut kw_idx = None;
    for kw in ["theorem ", "lemma ", "def "] {
        if let Some(idx) = before.rfind(kw) {
            if kw_idx.map_or(true, |prev| idx > prev) {
                kw_idx = Some(idx);
            }
        }
    }
    let kw_idx = kw_idx?;

    // Extract: skip "theorem name" to get just the signature
    let after_kw = &content[kw_idx..by_idx];
    // Skip keyword ("theorem ", "lemma ", "def ")
    let after_keyword = after_kw
        .trim_start_matches("theorem ")
        .trim_start_matches("lemma ")
        .trim_start_matches("def ");
    // Skip the name (first non-whitespace word)
    let after_name = after_keyword
        .trim_start()
        .find(|c: char| c.is_whitespace() || c == '(' || c == ':' || c == '[' || c == '{')
        .map(|i| &after_keyword.trim_start()[i..])
        .unwrap_or("");
    let signature = after_name.trim();

    // Find last top-level ":"
    let mut depth = 0i32;
    let mut colon_pos = None;
    for (i, c) in signature.char_indices().rev() {
        match c {
            ')' | ']' | '}' => depth += 1,
            '(' | '[' | '{' => depth -= 1,
            ':' if depth == 0 => {
                // Check not ":="
                if signature[i..].starts_with(":=") {
                    continue;
                }
                colon_pos = Some(i);
                break;
            }
            _ => {}
        }
    }
    let colon_pos = colon_pos?;

    let params = signature[..colon_pos].trim();
    let conclusion = signature[colon_pos + 1..].trim();

    if params.is_empty() {
        Some(conclusion.to_string())
    } else {
        Some(format!("forall {params}, {conclusion}"))
    }
}

fn extract_goal_at_line(lean_output: &str, target_line: usize) -> String {
    // Look for "unsolved goals" message near the target line
    let mut capturing = false;
    let mut goal_lines = Vec::new();

    for line in lean_output.lines() {
        // Match lines like "file.lean:15:2: error: unsolved goals"
        if line.contains("unsolved goals") {
            if let Some(line_num) = line.split(':').nth(1).and_then(|s| s.parse::<usize>().ok()) {
                if (line_num as isize - target_line as isize).unsigned_abs() <= 3 {
                    capturing = true;
                    continue;
                }
            }
        }
        if capturing {
            let trimmed = line.trim();
            // Stop at next diagnostic or empty section
            if trimmed.is_empty() || (trimmed.contains(".lean:") && trimmed.contains("error")) {
                break;
            }
            // Skip the "⊢" prefix line
            if trimmed.starts_with("⊢") {
                goal_lines.push(trimmed.trim_start_matches('⊢').trim().to_string());
            } else if !goal_lines.is_empty() {
                // Continuation of the goal type
                goal_lines.push(trimmed.to_string());
            }
        }
    }
    goal_lines.join(" ")
}
