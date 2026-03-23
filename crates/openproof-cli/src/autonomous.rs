//! Autonomous proof-search loop — TUI-driven functions.
//!
//! `schedule_autonomous_tick` and `run_autonomous_step` drive the
//! autonomous loop inside the interactive TUI via `AppEvent::AutonomousTick`.
//!
//! The headless CLI entry point (`openproof run`) lives in
//! `autonomous_headless.rs`.

use crate::helpers::{
    autonomous_stop_reason, autonomous_stop_reason_with_mode, best_hidden_branch,
    current_foreground_branch, persist_current_session, persist_write,
    should_promote_hidden_branch,
};
use crate::turn_handling::{
    ensure_hidden_agent_branch, start_agent_branch_turn, start_branch_verification,
};
use anyhow::Result;
use openproof_core::{AppEvent, AppState, AutonomousRunPatch};
use openproof_lean::lsp_mcp::LeanLspMcp;
use openproof_model::{run_codex_turn, CodexTurnRequest, TurnMessage};
use openproof_protocol::{AgentRole, AgentStatus, BranchQueueState, SearchStrategy};
use openproof_search::config::TacticSearchConfig;
use openproof_search::search::best_first_search;
use openproof_store::AppStore;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

/// Run a quick "research" model turn to gather relevant Mathlib lemmas and techniques.
/// Returns the model's response text, to be included in the prover's context.
async fn research_turn(target: &str, failed_summary: &str) -> Option<String> {
    let prompt = format!(
        "I need to prove the following in Lean 4 with Mathlib:\n{target}\n\n\
         A previous approach failed:\n{failed_summary}\n\n\
         List the 5 most relevant Mathlib lemma names (fully qualified) for this goal. \
         For each, give the exact name and type signature. \
         Also suggest 2-3 alternative proof strategies. \
         Be concrete -- give actual Mathlib names, not guesses. Use #check if unsure."
    );
    let messages = vec![
        TurnMessage::chat("system", "You are a Mathlib expert. Return ONLY concrete lemma names and type signatures. No prose."),
        TurnMessage::chat("user", prompt),
    ];
    run_codex_turn(CodexTurnRequest {
        session_id: "research",
        messages: &messages,
        model: "gpt-5.4",
        reasoning_effort: "medium",
        include_tools: false,
    })
    .await
    .ok()
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
    if let Some(reason) = autonomous_stop_reason_with_mode(&session, session.proof.full_autonomous) {
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

    let target = session
        .proof
        .accepted_target
        .clone()
        .or(session.proof.formal_target.clone())
        .ok_or_else(|| {
            "Set or accept a formal target before running autonomous search.".to_string()
        })?;

    // Ensure active_node_id is always set. Without it, verification and
    // branch context building all fail silently.
    if state.current_session().map(|s| s.proof.active_node_id.is_none()).unwrap_or(false) {
        if let Some(s) = state.current_session_mut() {
            if let Some(first) = s.proof.nodes.first() {
                eprintln!("[auto] Setting active_node_id to first node: {}", first.label);
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
            persist_write(tx.clone(), store.clone(), openproof_core::PendingWrite { session });
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
        let verification_session = state.current_session().cloned()
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
        // BACKTRACKING: After 3+ failed repairs, abandon this approach entirely.
        // attempt_count 3-5: try a different proof strategy
        // attempt_count 6+: decompose into sub-lemmas
        if basis.attempt_count >= 6 {
            // DECOMPOSITION: break into sub-lemmas
            actions.push(format!(
                "Decomposing: {} failed repairs. Breaking into sub-lemmas.",
                basis.attempt_count
            ));

            let decompose_context = format!(
                "The proof of {} has failed {} times with both the original and alternative approaches.\n\n\
                 DECOMPOSE this goal into 2-4 independent sub-lemmas. For each sub-lemma:\n\
                 1. Emit LEMMA: <label> :: <Lean type signature>\n\
                 2. Give a brief justification of why this sub-lemma helps\n\
                 3. Sketch how the sub-lemmas compose into the final proof\n\n\
                 The sub-lemmas should be EASIER to prove individually than the full goal.\n\
                 Each will be verified independently by Lean.\n\n\
                 Current target: {target}\n\nFailed approach summary:\n{}",
                target, basis.attempt_count, basis.summary,
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
                backtrack_context.push_str("\n\nLean DID find these correct facts -- use them even in the new approach:\n");
                for fact in &grounding {
                    backtrack_context.push_str(&format!("  {fact}\n"));
                }
            }

            // Include failed path history
            let target_label = latest_session.proof.nodes.first()
                .map(|n| n.label.as_str())
                .unwrap_or(&latest_session.title);
            if let Ok(failed) = store.failed_attempts_for_target(target_label, 5) {
                if !failed.is_empty() {
                    backtrack_context.push_str("\n\nAll previously failed approaches (do NOT repeat ANY of these):\n");
                    for (class, snippet, diag) in &failed {
                        backtrack_context.push_str(&format!("  [{class}] {snippet}\n    -> {diag}\n"));
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
        let current_content = latest_session.proof.last_rendered_scratch
            .as_deref()
            .or_else(|| latest_session.proof.nodes.first().map(|n| n.content.as_str()))
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
                 If you must rewrite, use a ```lean code block instead.\n\n"
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
                latest_session.proof.nodes.first().unwrap_or(&openproof_protocol::ProofNode::default()),
            );
            if let Ok(goals) = openproof_lean::extract_sorry_goals(&project_dir, &rendered) {
                if !goals.is_empty() {
                    repair_context.push_str("\n\nUnsolved goals at sorry points:\n");
                    for (line, goal) in &goals {
                        repair_context.push_str(&format!("  Line {line}: {goal}\n"));
                    }

                    // Premise retrieval: search corpus for lemmas matching goal types
                    // This is done synchronously via FTS for now; vector search is async
                    let goal_query = goals.iter()
                        .map(|(_, g)| g.as_str())
                        .collect::<Vec<_>>()
                        .join(" ");
                    if let Ok(premises) = store.search_verified_corpus(&goal_query, 5) {
                        if !premises.is_empty() {
                            repair_context.push_str("\n\nRelevant verified premises from corpus:\n");
                            for (label, statement, _vis) in &premises {
                                repair_context.push_str(&format!("  {label} :: {statement}\n"));
                            }
                        }
                    }
                }
            }

            if basis.attempt_count >= 2 {
                if let Ok(suggestions) = openproof_lean::run_tactic_suggestions(
                    &project_dir, &rendered, "exact?",
                ) {
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
        let target_label = latest_session.proof.nodes.first()
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
        if matches!(strategy, SearchStrategy::Hybrid | SearchStrategy::TacticSearch) {
            spawn_tactic_search_for_sorrys(
                tx.clone(),
                &latest_session,
                &store,
            );
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
            let lean_files: Vec<_> = files.iter()
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
        if let Some(summary) = latest_session.proof.strategy_summary.as_ref().filter(|s| !s.trim().is_empty()) {
            ctx.push_str(&format!("Strategy: {summary}\n\n"));
        }
        // Include past failed attempts so branches don't repeat them
        let target_label = latest_session.proof.nodes.first()
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
        // Explicit tool instructions
        ctx.push_str(
            "INSTRUCTIONS: Use tools to work on this code.\n\
             1. file_read to see current files with line numbers\n\
             2. file_patch to modify code (fill sorrys, fix errors)\n\
             3. lean_verify to check your changes compile\n\
             Do NOT output code as text. Use file_patch tool.\n\n"
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
        let description = format!("{branch_context}Produce an alternate Lean proof candidate for {target}.");
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
                    AppEvent::AppendBranchAssistant { branch_id, content, used_tools } => {
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
                        eprintln!("[run] {title}: {}", &content[..content.len().min(200)]);
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
                                eprintln!(
                                    "[run] Branch {} finished with no lean candidate.",
                                    bid
                                );
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

/// Spawn best-first tactic search tasks for each sorry position in the active
/// node's content. Each sorry gets its own search task, running in parallel
/// with any agentic branches. Results come back as `TacticSearchComplete` events.
fn spawn_tactic_search_for_sorrys(
    tx: mpsc::UnboundedSender<AppEvent>,
    session: &openproof_protocol::SessionSnapshot,
    store: &AppStore,
) {
    let content = session.proof.nodes.first()
        .map(|n| n.content.as_str())
        .unwrap_or("");
    if content.trim().is_empty() {
        return;
    }

    let sorry_positions = openproof_lean::find_sorry_positions(content);
    if sorry_positions.is_empty() {
        return;
    }

    let node_id = session.proof.active_node_id.clone().unwrap_or_default();
    let project_dir = crate::helpers::resolve_lean_project_dir();
    let workspace_dir = store.workspace_dir(&session.id);

    // Write the current content to a temp file for the LSP to read
    let scratch_path = workspace_dir.join("Scratch.lean");
    if let Some(rendered) = session.proof.last_rendered_scratch.as_deref() {
        let _ = std::fs::write(&scratch_path, rendered);
    } else if !content.trim().is_empty() {
        let rendered = openproof_lean::render_node_scratch(
            session,
            session.proof.nodes.first().unwrap(),
        );
        let _ = std::fs::write(&scratch_path, &rendered);
    }

    // Spawn MCP client for the search tasks
    let lsp_mcp = match LeanLspMcp::spawn(&project_dir) {
        Ok(client) => Arc::new(Mutex::new(client)),
        Err(e) => {
            eprintln!("[tactic-search] Cannot spawn lean-lsp-mcp: {e}");
            return;
        }
    };

    let config = TacticSearchConfig::default();

    for (line, col) in sorry_positions {
        let tx = tx.clone();
        let node_id = node_id.clone();
        let lsp = lsp_mcp.clone();
        let scratch = scratch_path.clone();
        let config = config.clone();

        tokio::task::spawn_blocking(move || {
            eprintln!("[tactic-search] Searching at line {line}, col {col}");

            // Simple propose function: returns common automation tactics.
            // In a full implementation this would call the LLM.
            let propose_fn: openproof_search::search::ProposeFn = Box::new(
                move |_goal: &str, _context: &str, k: usize| {
                    // Standard Lean automation tactics that often close goals
                    let mut tactics = vec![
                        "simp".to_string(),
                        "omega".to_string(),
                        "ring".to_string(),
                        "norm_num".to_string(),
                        "linarith".to_string(),
                        "aesop".to_string(),
                        "decide".to_string(),
                        "trivial".to_string(),
                        "exact?".to_string(),
                        "apply?".to_string(),
                        "simp_all".to_string(),
                        "tauto".to_string(),
                        "contradiction".to_string(),
                        "norm_cast".to_string(),
                        "positivity".to_string(),
                        "gcongr".to_string(),
                    ];
                    tactics.truncate(k);
                    Ok(tactics)
                },
            );

            match best_first_search(
                &lsp, &propose_fn, &scratch, line,
                "", // retrieval context
                &config,
            ) {
                Ok(result) => {
                    let (solved, tactics) = match &result {
                        openproof_search::config::SearchResult::Solved { tactics, .. } => {
                            (true, tactics.clone())
                        }
                        openproof_search::config::SearchResult::Partial { tactics, .. } => {
                            (false, tactics.clone())
                        }
                        _ => (false, vec![]),
                    };
                    if !tactics.is_empty() {
                        eprintln!(
                            "[tactic-search] Line {line}: {} (tactics: {})",
                            if solved { "SOLVED" } else { "partial" },
                            tactics.join("; ")
                        );
                    }
                    let _ = tx.send(AppEvent::TacticSearchComplete {
                        node_id,
                        sorry_line: line,
                        solved,
                        tactics,
                    });
                }
                Err(e) => {
                    eprintln!("[tactic-search] Line {line} error: {e}");
                }
            }
        });
    }
}
