//! Sub-command handlers for `/autonomous`, `/answer`, `/theorem`, `/lemma`, `/verify`.

use crate::helpers::{emit_local_notice, parse_statement_args, persist_write};
use crate::turn_handling::handle_submission;
use openproof_core::{AppEvent, AppState, AutonomousRunPatch, PendingWrite};
use openproof_protocol::ProofNodeKind;
use openproof_store::AppStore;
use std::{env, path::PathBuf};
use tokio::sync::mpsc;

pub fn cmd_autonomous(
    tx: mpsc::UnboundedSender<AppEvent>,
    state: &mut AppState,
    store: AppStore,
    arg_text: &str,
) {
    let subcommand = if arg_text.is_empty() { "status" } else { arg_text };
    match subcommand {
        "status" => {
            let content = state
                .current_session()
                .map(|session| {
                    [
                        format!("Phase: {}", session.proof.phase),
                        format!(
                            "Running: {}",
                            if session.proof.is_autonomous_running {
                                "yes"
                            } else {
                                "no"
                            }
                        ),
                        format!(
                            "Accepted target: {}",
                            session
                                .proof
                                .accepted_target
                                .clone()
                                .or(session.proof.formal_target.clone())
                                .unwrap_or_else(|| "none".to_string())
                        ),
                        format!("Branches: {}", session.proof.branches.len()),
                        format!("Hidden branches: {}", session.proof.hidden_branch_count),
                        format!(
                            "Best hidden: {}",
                            session
                                .proof
                                .hidden_best_branch_id
                                .clone()
                                .unwrap_or_else(|| "none".to_string())
                        ),
                        format!("Iteration: {}", session.proof.autonomous_iteration_count),
                        format!(
                            "Started: {}",
                            session
                                .proof
                                .autonomous_started_at
                                .clone()
                                .unwrap_or_else(|| "never".to_string())
                        ),
                        format!(
                            "Last progress: {}",
                            session
                                .proof
                                .autonomous_last_progress_at
                                .clone()
                                .unwrap_or_else(|| "never".to_string())
                        ),
                        format!(
                            "Pause reason: {}",
                            session
                                .proof
                                .autonomous_pause_reason
                                .clone()
                                .unwrap_or_else(|| "none".to_string())
                        ),
                        format!(
                            "Stop reason: {}",
                            session
                                .proof
                                .autonomous_stop_reason
                                .clone()
                                .unwrap_or_else(|| "none".to_string())
                        ),
                        format!(
                            "Foreground branch: {}",
                            session
                                .proof
                                .active_foreground_branch_id
                                .clone()
                                .unwrap_or_else(|| "none".to_string())
                        ),
                        format!(
                            "Strategy: {}",
                            session
                                .proof
                                .strategy_summary
                                .clone()
                                .unwrap_or_else(|| "none".to_string())
                        ),
                    ]
                    .join("\n")
                })
                .unwrap_or_else(|| "No active session.".to_string());
            emit_local_notice(tx, state, store, "Autonomous", content);
        }
        "start" => {
            let session = match state.current_session().cloned() {
                Some(session) => session,
                None => {
                    emit_local_notice(
                        tx,
                        state,
                        store,
                        "Autonomous Error",
                        "No active session.".to_string(),
                    );
                    return;
                }
            };
            if let Some(reason) =
                crate::helpers::autonomous_stop_reason(&session).filter(|reason| {
                    !reason.contains("completed the current proof run")
                })
            {
                emit_local_notice(tx, state, store, "Autonomous Error", reason);
                return;
            }
            let now = chrono::Utc::now().to_rfc3339();
            match state.set_autonomous_run_state(AutonomousRunPatch {
                is_autonomous_running: Some(true),
                autonomous_started_at: Some(Some(
                    session
                        .proof
                        .autonomous_started_at
                        .clone()
                        .unwrap_or(now.clone()),
                )),
                autonomous_last_progress_at: Some(
                    session
                        .proof
                        .autonomous_last_progress_at
                        .clone()
                        .or(Some(now)),
                ),
                autonomous_pause_reason: Some(None),
                autonomous_stop_reason: Some(None),
                ..AutonomousRunPatch::default()
            }) {
                Ok(write) => {
                    persist_write(tx.clone(), store.clone(), write);
                    let _ = tx.send(AppEvent::AutonomousTick);
                    emit_local_notice(
                        tx,
                        state,
                        store,
                        "Autonomous",
                        "Autonomous proof loop started.".to_string(),
                    );
                }
                Err(error) => emit_local_notice(tx, state, store, "Autonomous Error", error),
            }
        }
        "stop" => match state.set_autonomous_run_state(AutonomousRunPatch {
            is_autonomous_running: Some(false),
            autonomous_pause_reason: Some(Some("Interrupted by user.".to_string())),
            autonomous_stop_reason: Some(None),
            ..AutonomousRunPatch::default()
        }) {
            Ok(write) => {
                persist_write(tx.clone(), store.clone(), write);
                emit_local_notice(
                    tx,
                    state,
                    store,
                    "Autonomous",
                    "Autonomous proof loop paused.".to_string(),
                );
            }
            Err(error) => emit_local_notice(tx, state, store, "Autonomous Error", error),
        },
        "step" => {
            match crate::autonomous::run_autonomous_step(tx.clone(), store.clone(), state) {
                Ok(message) => emit_local_notice(tx, state, store, "Autonomous", message),
                Err(error) => emit_local_notice(tx, state, store, "Autonomous Error", error),
            }
        }
        _ => emit_local_notice(
            tx,
            state,
            store,
            "Autonomous Usage",
            "Usage: /autonomous status|start|stop|step".to_string(),
        ),
    }
}

pub fn cmd_answer(
    tx: mpsc::UnboundedSender<AppEvent>,
    state: &mut AppState,
    store: AppStore,
    arg_text: &str,
) {
    let Some(question) = state
        .current_session()
        .and_then(|session| session.proof.pending_question.clone())
    else {
        emit_local_notice(
            tx,
            state,
            store,
            "Answer Error",
            "No pending clarification question.".to_string(),
        );
        return;
    };
    if arg_text.is_empty() {
        emit_local_notice(
            tx,
            state,
            store,
            "Answer Usage",
            "Usage: /answer <option-id|text>".to_string(),
        );
        return;
    }
    let reply_text = question
        .options
        .iter()
        .find(|option| option.id == arg_text)
        .map(|option| {
            if option.formal_target.trim().is_empty() {
                option.label.clone()
            } else {
                option.formal_target.clone()
            }
        })
        .unwrap_or_else(|| arg_text.to_string());
    if let Some(submitted) = state.submit_text(reply_text) {
        persist_write(
            tx.clone(),
            store.clone(),
            PendingWrite {
                session: submitted.session_snapshot.clone(),
            },
        );
        handle_submission(tx, store, state, submitted);
    } else {
        emit_local_notice(
            tx,
            state,
            store,
            "Answer Error",
            "Could not submit clarification answer.".to_string(),
        );
    }
}

pub fn apply_statement_command(
    tx: mpsc::UnboundedSender<AppEvent>,
    state: &mut AppState,
    store: AppStore,
    kind: ProofNodeKind,
    arg_text: &str,
) {
    let Some((label, statement)) = parse_statement_args(arg_text) else {
        let usage = match kind {
            ProofNodeKind::Theorem => "Usage: /theorem <label> :: <statement>",
            ProofNodeKind::Lemma => "Usage: /lemma <label> :: <statement>",
            _ => "Usage: /<kind> <label> :: <statement>",
        };
        emit_local_notice(tx, state, store, "Usage", usage.to_string());
        return;
    };
    match state.add_proof_node(kind, &label, &statement) {
        Ok(write) => persist_write(tx, store, write),
        Err(error) => emit_local_notice(tx, state, store, "Statement Error", error),
    }
}

pub fn start_verify_active_node(
    tx: mpsc::UnboundedSender<AppEvent>,
    state: &mut AppState,
    store: AppStore,
) {
    if state.verification_in_flight {
        emit_local_notice(
            tx,
            state,
            store,
            "Verify Busy",
            "Lean verification is already running.".to_string(),
        );
        return;
    }

    let session = match state.current_session().cloned() {
        Some(session) => session,
        None => {
            emit_local_notice(tx, state, store, "Verify Error", "No active session.".to_string());
            return;
        }
    };

    let mut verification_session = session.clone();
    if let Some(active_branch_id) = session.proof.active_branch_id.as_deref() {
        if let Some(branch) = session
            .proof
            .branches
            .iter()
            .find(|branch| branch.id == active_branch_id)
        {
            if !branch.lean_snippet.trim().is_empty() {
                if let Some(focus_node_id) = branch
                    .focus_node_id
                    .as_deref()
                    .or(session.proof.active_node_id.as_deref())
                {
                    verification_session.proof.active_node_id = Some(focus_node_id.to_string());
                    if let Some(node) = verification_session
                        .proof
                        .nodes
                        .iter_mut()
                        .find(|node| node.id == focus_node_id)
                    {
                        node.content = branch.lean_snippet.clone();
                    }
                }
            }
        }
    }

    if verification_session.proof.active_node_id.is_none() {
        emit_local_notice(
            tx,
            state,
            store,
            "Verify Error",
            "No active proof node is focused.".to_string(),
        );
        return;
    }

    if let Some(write) = state.apply(AppEvent::LeanVerifyStarted) {
        persist_write(tx.clone(), store.clone(), write);
    }
    let project_dir = env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("lean");
    let tx_verify = tx.clone();
    tokio::spawn(async move {
        let outcome = tokio::task::spawn_blocking(move || {
            openproof_lean::verify_active_node(&project_dir, &verification_session)
        })
        .await
        .ok()
        .and_then(|r| r.ok());
        match outcome {
            Some(result) => {
                let _ = tx_verify.send(AppEvent::LeanVerifyFinished(result));
            }
            None => {
                let _ = tx_verify.send(AppEvent::AppendNotice {
                    title: "Verify Error".to_string(),
                    content: "Lean verification crashed.".to_string(),
                });
            }
        }
    });
}
