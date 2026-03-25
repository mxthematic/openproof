//! Headless autonomous proof-search entry point (`openproof run`).
//!
//! This module owns `run_autonomous`, the non-interactive CLI path.
//! The TUI-driven step functions (`schedule_autonomous_tick`,
//! `run_autonomous_step`) live in `autonomous.rs`.

use crate::autonomous::{drain_until_settled, run_autonomous_step};
use crate::helpers::{
    autonomous_stop_reason, extract_lean_blocks_from_text, persist_write,
    resolve_lean_project_dir,
};
use crate::system_prompt::build_turn_messages_with_retrieval;
use crate::turn_handling::run_agentic_loop;
use anyhow::{bail, Result};
use openproof_core::{AppEvent, AppState, AutonomousRunPatch};
use openproof_model::{load_auth_summary, sync_auth_from_codex_cli};
use openproof_protocol::{MessageRole, ProofNodeKind};
use openproof_store::{AppStore, StorePaths};
use std::path::PathBuf;
use tokio::sync::mpsc;

pub async fn run_autonomous(
    launch_cwd: PathBuf,
    problem: String,
    label: Option<String>,
    resume: Option<String>,
) -> Result<()> {
    let store = AppStore::open(StorePaths::detect()?)?;
    let _ = store.import_legacy_sessions();
    let workspace_label = launch_cwd
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "workspace".to_string());

    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();

    let sessions = store.list_sessions()?;
    let wr = Some(launch_cwd.to_string_lossy().to_string());
    let wl = Some(workspace_label.clone());
    let mut state = AppState::new(sessions, String::new(), wr.clone(), wl.clone());

    // Resume existing session or create new one
    if let Some(ref session_id) = resume {
        eprintln!("[run] Resuming session: {session_id}");
        if let Err(e) = state.switch_session(session_id) {
            bail!("Could not resume session {session_id}: {e}");
        }
        // Reset phase so autonomous loop doesn't stop immediately
        if let Some(s) = state.current_session_mut() {
            if s.proof.phase == "done" || s.proof.phase == "blocked" {
                eprintln!("[run] Resetting phase from '{}' to 'proving' for continuation", s.proof.phase);
                s.proof.phase = "proving".to_string();
            }
            s.proof.is_autonomous_running = false; // will be set by the loop
            s.proof.full_autonomous = true; // never stop until ALL nodes verified
            s.proof.autonomous_iteration_count = 0;
            s.proof.autonomous_pause_reason = None;
            s.proof.autonomous_stop_reason = None;
            // Reset all stalled branches so they can be retried
            for branch in &mut s.proof.branches {
                if branch.status == openproof_protocol::AgentStatus::Blocked
                    || branch.status == openproof_protocol::AgentStatus::Error
                {
                    branch.status = openproof_protocol::AgentStatus::Done;
                }
            }
        }
        let session = state.current_session().cloned().unwrap();
        eprintln!("[run] Session: {} ({})", session.title, session.id);
        eprintln!("[run] Phase: {}, Nodes: {}, Branches: {}",
            session.proof.phase, session.proof.nodes.len(), session.proof.branches.len());

        // If a new problem text was given, submit it as a continuation message
        if !problem.trim().is_empty() {
            let _ = state.submit_text(problem.clone());
            if let Some(session) = state.current_session().cloned() {
                let s = store.clone();
                let _ = tokio::task::spawn_blocking(move || s.save_session(&session)).await;
            }
        }
    } else {
        let title = label.unwrap_or_else(|| {
            let preview: String = problem.chars().take(60).collect();
            format!("auto: {preview}")
        });
        let write = state.create_session(Some(&title));
        persist_write(tx.clone(), store.clone(), write);

        // Apply config: set cloud mode if configured
        if let Some(config) = crate::setup::load_config() {
            if config.corpus_mode == "cloud" {
                if let Some(s) = state.current_session_mut() {
                    s.cloud.share_mode = openproof_protocol::ShareMode::Community;
                    s.cloud.sync_enabled = true;
                }
            }
            if let Some(url) = &config.corpus_url {
                std::env::set_var("OPENPROOF_CORPUS_URL", url);
                std::env::set_var("OPENPROOF_ENABLE_REMOTE_CORPUS", "1");
            }
        }
    }

    // Load auth
    eprintln!("[run] Loading auth...");
    match sync_auth_from_codex_cli() {
        Ok(Some(summary)) => {
            let _ = state.apply(AppEvent::AuthLoaded(summary));
        }
        _ => {
            if let Ok(summary) = load_auth_summary() {
                let _ = state.apply(AppEvent::AuthLoaded(summary));
            }
        }
    }
    if !state.auth.logged_in {
        bail!("Not authenticated. Run `openproof login` first.");
    }
    eprintln!(
        "[run] Auth: {} ({})",
        state.auth.email.as_deref().unwrap_or("unknown"),
        state.auth.plan.as_deref().unwrap_or("?")
    );

    // Load lean health
    let lean_dir = resolve_lean_project_dir();
    let lean_dir_clone = lean_dir.clone();
    if let Ok(health) =
        tokio::task::spawn_blocking(move || openproof_lean::detect_lean_health(&lean_dir_clone))
            .await?
    {
        let _ = state.apply(AppEvent::LeanLoaded(health));
    }
    eprintln!(
        "[run] Lean: ok={}, version={}",
        state.lean.ok,
        state.lean.lean_version.as_deref().unwrap_or("?")
    );

    // Submit problem and run initial agentic turn (skip if resuming)
    if resume.is_some() {
        eprintln!("[run] Resuming -- skipping initial turn.");
    }
    let should_submit = resume.is_none();
    eprintln!("[run] Problem: {problem}");
    let submitted = if should_submit { state.submit_text(problem.clone()) } else { None };
    if let Some(_input) = submitted {
        if let Some(session) = state.current_session().cloned() {
            let s = store.clone();
            let _ = tokio::task::spawn_blocking(move || s.save_session(&session)).await;
        }

        let session = state.current_session().cloned().unwrap();
        let messages = build_turn_messages_with_retrieval(&store, Some(&session)).await;
        eprintln!("[run] Running initial agentic turn (with tools: lean_verify, file_write, corpus_search, etc.)...");
        let _ = state.apply(AppEvent::TurnStarted);

        // Spawn Pantograph for fast tactic testing (~18s Mathlib import, then 3ms/tactic)
        let project_dir = crate::helpers::resolve_lean_project_dir();
        let prover: Option<openproof_lean::proof_tree::SharedProver> =
            openproof_lean::proof_tree::SessionProver::spawn(&project_dir)
                .map(|sp| {
                    eprintln!("[run] Pantograph ready (Mathlib loaded)");
                    std::sync::Arc::new(std::sync::Mutex::new(sp))
                })
                .map_err(|e| eprintln!("[run] Pantograph not available: {e}"))
                .ok();

        run_agentic_loop(
            tx.clone(),
            store.clone(),
            &session.id,
            messages,
            &session,
            prover.clone(),
        )
        .await;

        // Drain events produced by the agentic loop
        while let Ok(event) = rx.try_recv() {
            match &event {
                AppEvent::AppendAssistant(text) => {
                    eprintln!("[run] Assistant ({} chars, {} lines)", text.len(), text.lines().count());
                    for line in text.lines().take(15) {
                        eprintln!("  | {line}");
                    }
                }
                AppEvent::AppendNotice { title, content } => {
                    eprintln!("[run] {title}: {}", content.chars().take(200).collect::<String>());
                }
                AppEvent::ToolCallReceived { tool_name, .. } => {
                    eprintln!("[run] TOOL: {tool_name}");
                }
                AppEvent::ToolResultReceived { tool_name, success, output, .. } => {
                    eprintln!("[run] RESULT: {tool_name} -> {} ({})",
                        if *success { "ok" } else { "FAIL" },
                        output.chars().take(100).collect::<String>());
                }
                _ => {}
            }
            let _ = state.apply(event);
        }
        let _ = state.apply(AppEvent::TurnFinished);
        if let Some(session) = state.current_session().cloned() {
            let s = store.clone();
            let _ = tokio::task::spawn_blocking(move || s.save_session(&session)).await;
        }
    }

    // Report extracted state
    let session = state.current_session().cloned().unwrap();
    eprintln!("[run] Phase: {}", session.proof.phase);
    eprintln!("[run] Formal target: {:?}", session.proof.formal_target);
    eprintln!("[run] Accepted target: {:?}", session.proof.accepted_target);
    eprintln!("[run] Nodes: {}", session.proof.nodes.len());
    for node in &session.proof.nodes {
        eprintln!(
            "[run]   {} [{:?}]: {}",
            node.label, node.status, node.statement
        );
    }

    if session.proof.formal_target.is_none()
        && session.proof.accepted_target.is_none()
        && session.proof.nodes.is_empty()
    {
        eprintln!("[run] No target extracted. Adding theorem node from problem.");
        let node_label = state.current_session().map(|s| s.title.clone()).unwrap_or_else(|| "Goal".to_string());
        let _ = state.add_proof_node(ProofNodeKind::Theorem, &node_label, &problem);
    }

    // Auto-accept target if none accepted yet
    let session = state.current_session().cloned().unwrap();
    if session.proof.accepted_target.is_none() {
        let target = session
            .proof
            .formal_target
            .clone()
            .or_else(|| session.proof.nodes.first().map(|n| n.statement.clone()))
            .unwrap_or_else(|| problem.clone());
        eprintln!(
            "[run] Auto-accepting target: {}",
            target.chars().take(100).collect::<String>()
        );
        if let Some(s) = state.current_session_mut() {
            s.proof.accepted_target = Some(target);
            s.proof.phase = "proving".to_string();
        }
        if let Some(session) = state.current_session().cloned() {
            let s = store.clone();
            let _ = tokio::task::spawn_blocking(move || s.save_session(&session)).await;
        }
    }

    // Sync workspace content to active node AFTER nodes are created and target is set.
    // This must happen here (not earlier) because nodes may not exist until
    // the "No target extracted" fallback creates them above.
    {
        let session_id = state.current_session().map(|s| s.id.clone()).unwrap_or_default();
        // Read ALL .lean files from workspace (model may use Main.lean, Defs.lean, etc.)
        let ws_dir = store.workspace_dir(&session_id);
        let mut all_lean = String::new();
        if let Ok(files) = store.list_workspace_files(&session_id) {
            for (path, _) in &files {
                if path.ends_with(".lean") && !path.contains("history/") {
                    if let Ok(content) = std::fs::read_to_string(ws_dir.join(path)) {
                        if !all_lean.is_empty() {
                            all_lean.push_str("\n\n");
                        }
                        all_lean.push_str(&content);
                    }
                }
            }
        }
        if all_lean.is_empty() {
            if let Some(scratch) = store.read_scratch(&session_id) {
                all_lean = scratch;
            }
        }
        if !all_lean.trim().is_empty() {
            if let Some(s) = state.current_session_mut() {
                // Parse declarations and rebuild nodes from workspace code
                let parsed = openproof_lean::parse_lean_declarations(&all_lean);
                if !parsed.is_empty() {
                    let now = chrono::Utc::now().to_rfc3339();
                    let parsed_nodes = openproof_lean::declarations_to_proof_nodes(&parsed, &s.id);
                    let old_statuses: std::collections::HashMap<String, openproof_protocol::ProofNodeStatus> =
                        s.proof.nodes.iter().map(|n| (n.label.clone(), n.status)).collect();

                    s.proof.nodes = parsed_nodes.iter().map(|pn| {
                        let mut node = pn.clone();
                        if let Some(&prev) = old_statuses.get(&node.label) {
                            if prev == openproof_protocol::ProofNodeStatus::Verified && !node.content.contains("sorry") {
                                node.status = prev;
                            }
                        }
                        if node.content.contains("sorry") {
                            node.status = openproof_protocol::ProofNodeStatus::Proving;
                        } else if !node.content.trim().is_empty() {
                            node.status = openproof_protocol::ProofNodeStatus::Proving;
                        }
                        node.updated_at = now.clone();
                        node
                    }).collect();

                    eprintln!("[run] Parsed {} declarations from workspace", s.proof.nodes.len());

                    if let Some(root) = s.proof.nodes.first() {
                        s.proof.root_node_id = Some(root.id.clone());
                    }
                }

                // Set active to first unverified root
                s.proof.active_node_id = s.proof.nodes.iter()
                    .find(|n| n.depth == 0 && n.status != openproof_protocol::ProofNodeStatus::Verified)
                    .or_else(|| s.proof.nodes.first())
                    .map(|n| n.id.clone());

                s.proof.last_rendered_scratch = Some(all_lean);
            }
            if let Some(session) = state.current_session().cloned() {
                let s = store.clone();
                let _ = tokio::task::spawn_blocking(move || s.save_session(&session)).await;
            }
        }
    }

    // Direct verification: check workspace files for compilable lean code.
    // Skip when resuming -- the existing code already verified, we want to push further.
    if resume.is_none() {
        let session = state.current_session().cloned().unwrap();

        // Read workspace .lean files directly (source of truth)
        let ws_dir = store.workspace_dir(&session.id);
        let mut lean_candidates: Vec<String> = Vec::new();
        if let Ok(files) = store.list_workspace_files(&session.id) {
            for (path, _) in &files {
                if path.ends_with(".lean") && !path.contains("history/") {
                    if let Ok(content) = std::fs::read_to_string(ws_dir.join(path)) {
                        if !content.trim().is_empty() && !lean_candidates.iter().any(|c| c == &content) {
                            lean_candidates.push(content);
                        }
                    }
                }
            }
        }

        // Also check node content and transcript as fallbacks
        for node in &session.proof.nodes {
            if !node.content.trim().is_empty() && !lean_candidates.iter().any(|c| c == &node.content) {
                lean_candidates.push(node.content.clone());
            }
        }
        if let Some(last_msg) = session.transcript.iter().rev()
            .find(|e| e.role == MessageRole::Assistant)
        {
            for block in extract_lean_blocks_from_text(&last_msg.content) {
                if !lean_candidates.iter().any(|c| c.trim() == block.trim()) {
                    lean_candidates.push(block);
                }
            }
        }

        if !lean_candidates.is_empty() {
            eprintln!(
                "[run] Found {} lean candidate(s), verifying directly...",
                lean_candidates.len()
            );
            let project_dir = resolve_lean_project_dir();
            for (idx, candidate) in lean_candidates.iter().enumerate() {
                eprintln!(
                    "[run] Verifying candidate {} ({} chars)",
                    idx + 1,
                    candidate.len()
                );

                if let Some(s) = state.current_session_mut() {
                    if let Some(node) = s.proof.nodes.first_mut() {
                        node.content = candidate.clone();
                    }
                }
                let verify_session = state.current_session().cloned().unwrap();
                let pd = project_dir.clone();

                let session_id = verify_session.id.clone();
                let rendered = openproof_lean::render_node_scratch(
                    &verify_session,
                    verify_session.proof.nodes.first().unwrap(),
                );
                let persistent_path = store
                    .write_scratch(&session_id, &rendered)
                    .ok()
                    .map(|(p, _)| p);

                let result = tokio::task::spawn_blocking(move || {
                    openproof_lean::verify_node_at(
                        &pd,
                        &verify_session,
                        verify_session.proof.nodes.first().unwrap(),
                        persistent_path.as_deref(),
                    )
                })
                .await
                .ok()
                .and_then(|r| r.ok());

                if let Some(result) = result {
                    if result.ok {
                        eprintln!("[run] *** DIRECT VERIFICATION SUCCEEDED ***");
                        let sr = store.clone();
                        let ss = state.current_session().cloned().unwrap();
                        let rr = result.clone();
                        let sr2 = store.clone();
                        let sid = ss.id.clone();
                        let _ = tokio::task::spawn_blocking(move || {
                            let res = sr.record_verification_result(&ss, &rr);
                            crate::helpers::populate_knowledge_graph(&sr2, &sid);
                            res
                        })
                        .await;

                        if let Some(s) = state.current_session_mut() {
                            if let Some(n) = s.proof.nodes.first_mut() {
                                n.status = openproof_protocol::ProofNodeStatus::Verified;
                            }
                            s.proof.phase = "done".to_string();
                            s.proof.status_line = "Verified on first attempt.".to_string();
                        }
                        if let Some(session) = state.current_session().cloned() {
                            let s = store.clone();
                            let _ =
                                tokio::task::spawn_blocking(move || s.save_session(&session))
                                    .await;
                        }

                        let session = state.current_session().cloned().unwrap();
                        eprintln!("\n[run] === Verified (direct) ===");
                        eprintln!("[run] Session: {}", session.title);
                        eprintln!("{candidate}");
                        let corpus = store.get_corpus_summary()?;
                        eprintln!(
                            "[run] Corpus: verified={}, user_verified={}",
                            corpus.verified_entry_count, corpus.user_verified_count
                        );
                        // Don't return early -- fall through to cloud sync below
                        break;
                    } else {
                        eprintln!("[run] Candidate {} failed:", idx + 1);
                        for line in result.stderr.lines().take(3) {
                            eprintln!("[run]   {line}");
                        }
                        let sr = store.clone();
                        let ss = state.current_session().cloned().unwrap();
                        let rr = result.clone();
                        let _ = tokio::task::spawn_blocking(move || {
                            sr.record_verification_result(&ss, &rr)
                        })
                        .await;
                    }
                }
            }
            eprintln!("[run] Direct verification did not succeed. Entering autonomous loop.");
        }
    }

    // Start autonomous loop in full mode (never stops until all nodes verified)
    if let Some(s) = state.current_session_mut() {
        s.proof.full_autonomous = true;
    }
    eprintln!("\n[run] === Starting autonomous loop (full mode) ===\n");
    if let Ok(write) = state.set_autonomous_run_state(AutonomousRunPatch {
        is_autonomous_running: Some(true),
        autonomous_iteration_count: Some(0),
        autonomous_started_at: Some(Some(chrono::Utc::now().to_rfc3339())),
        autonomous_pause_reason: Some(None),
        autonomous_stop_reason: Some(None),
        ..AutonomousRunPatch::default()
    }) {
        persist_write(tx.clone(), store.clone(), write);
    }

    let max_iterations = 100; // Open problems need many iterations
    for iteration in 1..=max_iterations {
        // Ensure active_node_id is set before each iteration
        if let Some(s) = state.current_session_mut() {
            if s.proof.active_node_id.is_none() {
                if let Some(first) = s.proof.nodes.first() {
                    s.proof.active_node_id = Some(first.id.clone());
                }
            }
        }

        let session = state.current_session().cloned().unwrap();

        // Headless mode: only stop when ALL nodes are truly verified (no sorry)
        let all_verified = !session.proof.nodes.is_empty()
            && session.proof.nodes.iter().all(|n| {
                n.status == openproof_protocol::ProofNodeStatus::Verified
                    && !n.content.contains("sorry")
            });
        if all_verified {
            eprintln!("[run] All proof nodes verified (no sorry)!");
            break;
        }
        if session.proof.pending_question.is_some() || session.proof.awaiting_clarification {
            eprintln!("[run] Paused for clarification.");
            break;
        }

        // Re-enable autonomous running if it got turned off by the step function
        if !session.proof.is_autonomous_running {
            if let Some(s) = state.current_session_mut() {
                s.proof.is_autonomous_running = true;
                s.proof.phase = "proving".to_string();
                s.proof.autonomous_pause_reason = None;
                s.proof.autonomous_stop_reason = None;
            }
        }

        eprintln!("\n[run] --- Iteration {iteration}/{max_iterations} ---");
        eprintln!(
            "[run] Phase={}, Branches={}, Nodes={}",
            session.proof.phase,
            session.proof.branches.len(),
            session.proof.nodes.len()
        );

        match run_autonomous_step(tx.clone(), store.clone(), &mut state) {
            Ok(summary) => {
                for line in summary.lines() {
                    eprintln!("[run] {line}");
                }
            }
            Err(reason) => {
                eprintln!("[run] Step error: {reason}");
                break;
            }
        }

        // Drain events until all branches settle
        drain_until_settled(tx.clone(), store.clone(), &mut state, &mut rx).await;

        // Sync workspace content after branch turns (branches may have written files)
        {
            let sid = state.current_session().map(|s| s.id.clone()).unwrap_or_default();
            let ws_dir = store.workspace_dir(&sid);
            let mut all_lean = String::new();
            if let Ok(files) = store.list_workspace_files(&sid) {
                for (path, _) in &files {
                    if path.ends_with(".lean") && !path.contains("history/") {
                        if let Ok(content) = std::fs::read_to_string(ws_dir.join(path)) {
                            if !all_lean.is_empty() {
                                all_lean.push_str("\n\n");
                            }
                            all_lean.push_str(&content);
                        }
                    }
                }
            }
            if all_lean.is_empty() {
                if let Some(scratch) = store.read_scratch(&sid) {
                    all_lean = scratch;
                }
            }
            if !all_lean.trim().is_empty() {
                if let Some(s) = state.current_session_mut() {
                    if let Some(node_id) = s.proof.active_node_id.clone() {
                        if let Some(node) = s.proof.nodes.iter_mut().find(|n| n.id == node_id) {
                            if node.content.trim().is_empty() || node.content != all_lean {
                                node.content = all_lean.clone();
                                // Reset verified status if new content has sorry
                                if all_lean.contains("sorry")
                                    && node.status == openproof_protocol::ProofNodeStatus::Verified
                                {
                                    node.status = openproof_protocol::ProofNodeStatus::Proving;
                                }
                            }
                        }
                    }
                }
            }
        }

        if let Some(session) = state.current_session().cloned() {
            let s = store.clone();
            let _ = tokio::task::spawn_blocking(move || s.save_session(&session)).await;
        }

        let session = state.current_session().cloned().unwrap();
        let verified: Vec<_> = session
            .proof
            .nodes
            .iter()
            .filter(|n| n.status == openproof_protocol::ProofNodeStatus::Verified)
            .collect();
        if !verified.is_empty() {
            eprintln!("\n[run] *** {} node(s) VERIFIED ***", verified.len());
            for node in &verified {
                eprintln!("[run] {}: {}", node.label, node.statement);
            }
            // In full_autonomous mode, don't stop on first verification.
            // Keep pushing -- verified sub-lemmas are progress but not the goal.
            let all_verified = session.proof.nodes.iter()
                .all(|n| {
                    n.status == openproof_protocol::ProofNodeStatus::Verified
                        && !n.content.contains("sorry")
                });
            if all_verified {
                eprintln!("[run] All nodes verified (no sorry) -- stopping.");
                break;
            }
            eprintln!("[run] Continuing -- not all nodes verified yet.");
        }
    }

    // Paper is auto-generated in persist_write on every save.
    // Force a final save to ensure paper_tex is populated.
    if let Some(session) = state.current_session().cloned() {
        persist_write(tx.clone(), store.clone(), openproof_core::PendingWrite { session });
    }

    let session = state.current_session().cloned().unwrap();
    eprintln!("\n[run] === Summary ===");
    eprintln!("[run] Session: {} ({})", session.title, session.id);
    eprintln!("[run] Phase: {}", session.proof.phase);
    eprintln!("[run] Iterations: {}", session.proof.autonomous_iteration_count);
    eprintln!("[run] Nodes: {}", session.proof.nodes.len());
    for n in &session.proof.nodes {
        eprintln!("[run]   {} [{:?}]", n.label, n.status);
    }
    eprintln!("[run] Branches: {}", session.proof.branches.len());
    for b in &session.proof.branches {
        eprintln!(
            "[run]   {} [{:?}] score={:.1} attempts={}",
            b.title, b.status, b.score, b.attempt_count
        );
    }
    let corpus = store.get_corpus_summary()?;
    eprintln!(
        "[run] Corpus: verified={}, user_verified={}, attempts={}",
        corpus.verified_entry_count, corpus.user_verified_count, corpus.attempt_log_count
    );

    let s = store.clone();
    let _ = tokio::task::spawn_blocking(move || s.save_session(&session)).await;

    // Auto-sync to cloud if enabled
    let session = state.current_session().cloned().unwrap();
    if session.cloud.sync_enabled && session.cloud.share_mode != openproof_protocol::ShareMode::Local {
        eprintln!("[run] Syncing to cloud corpus...");
        let corpus_mgr = openproof_corpus::CorpusManager::new(
            store.clone(),
            openproof_cloud::CloudCorpusClient::new(Default::default()),
            std::path::PathBuf::from("."),
        );
        match corpus_mgr.drain_sync_queue(session.cloud.share_mode, true, None).await {
            Ok(result) => {
                eprintln!("[run] Synced: {} sent, {} failed", result.sent, result.failed);
            }
            Err(e) => {
                eprintln!("[run] Sync error: {e}");
            }
        }
    }

    Ok(())
}

