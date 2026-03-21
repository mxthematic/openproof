//! Turn lifecycle: submitting prompts, spawning agent branches, and
//! launching branch Lean verification.
//!
//! A "turn" is one round-trip to the model (either the main session transcript
//! or a named agent branch).  This module owns the async spawning logic that
//! feeds `AppEvent`s back to the event loop.

use crate::helpers::{
    agent_role_label, branch_phase_for_role, emit_local_notice, persist_current_session,
    persist_write, resolve_lean_project_dir, summarize_branch_output, truncate,
};
use crate::system_prompt::{build_branch_turn_messages, build_turn_messages_with_retrieval};
use openproof_core::{AppEvent, AppState, PendingWrite, SubmittedInput};
use openproof_model::{run_codex_turn, run_codex_turn_streaming, CodexTurnRequest};
use openproof_protocol::{AgentRole, AgentStatus, BranchQueueState, SessionSnapshot};
use openproof_store::AppStore;
use tokio::sync::mpsc;

pub fn handle_submission(
    tx: mpsc::UnboundedSender<AppEvent>,
    store: AppStore,
    state: &mut AppState,
    submission: SubmittedInput,
) {
    if submission.raw_text.trim_start().starts_with('/') {
        crate::slash_commands::apply_local_command(tx, state, store, submission);
        return;
    }

    if state.turn_in_flight {
        let _ = tx.send(AppEvent::AppendNotice {
            title: "Busy".to_string(),
            content: "A model turn is already running. Wait for it to finish before submitting another prompt.".to_string(),
        });
        return;
    }

    let _ = state.apply(AppEvent::TurnStarted);
    let session_snapshot = submission.session_snapshot.clone();
    let tx_model = tx.clone();
    let store_for_model = store.clone();
    tokio::spawn(async move {
        let messages =
            build_turn_messages_with_retrieval(&store_for_model, Some(&session_snapshot)).await;
        let (delta_tx, mut delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let tx_stream = tx_model.clone();
        tokio::spawn(async move {
            while let Some(delta) = delta_rx.recv().await {
                let _ = tx_stream.send(AppEvent::StreamDelta(delta));
            }
        });
        let result = run_codex_turn_streaming(
            CodexTurnRequest {
                session_id: &submission.session_id,
                messages: &messages,
                model: "gpt-5.4",
                reasoning_effort: "high",
            },
            delta_tx,
        )
        .await;

        match result {
            Ok(_text) => {
                // Streaming text was accumulated via StreamDelta events.
                // TurnFinished will flush it into AppendAssistant.
            }
            Err(error) => {
                let _ = tx_model.send(AppEvent::AppendNotice {
                    title: "Assistant Error".to_string(),
                    content: error.to_string(),
                });
            }
        }
        let _ = tx_model.send(AppEvent::TurnFinished);
    });
}

pub fn start_agent_branch_turn(
    tx: mpsc::UnboundedSender<AppEvent>,
    store: AppStore,
    role: AgentRole,
    title: String,
    branch_id: String,
    _task_id: String,
    session_snapshot: SessionSnapshot,
) {
    tokio::spawn(async move {
        let messages =
            build_branch_turn_messages(&store, &session_snapshot, role, &title, &branch_id).await;
        let result = run_codex_turn(CodexTurnRequest {
            session_id: &branch_id,
            messages: &messages,
            model: "gpt-5.4",
            reasoning_effort: "high",
        })
        .await;

        match result {
            Ok(text) => {
                let content = if text.trim().is_empty() {
                    "The model returned no visible text.".to_string()
                } else {
                    text
                };
                let summary = summarize_branch_output(&content);
                let _ = tx.send(AppEvent::AppendBranchAssistant {
                    branch_id: branch_id.clone(),
                    content,
                });
                let _ = tx.send(AppEvent::FinishBranch {
                    branch_id,
                    status: AgentStatus::Done,
                    summary,
                    output: String::new(),
                });
            }
            Err(error) => {
                let message = error.to_string();
                let _ = tx.send(AppEvent::FinishBranch {
                    branch_id,
                    status: AgentStatus::Error,
                    summary: format!("Branch failed: {}", truncate(&message, 160)),
                    output: message,
                });
            }
        }
    });
}

pub fn start_branch_verification(
    tx: mpsc::UnboundedSender<AppEvent>,
    store: AppStore,
    session_snapshot: SessionSnapshot,
    branch_id: String,
    promote: bool,
) {
    let Some((verification_session, focus_node_id)) =
        build_branch_verification_session(&session_snapshot, &branch_id)
    else {
        let _ = tx.send(AppEvent::AppendNotice {
            title: "Verify Error".to_string(),
            content: format!("Branch {branch_id} has no Lean candidate to verify."),
        });
        return;
    };

    let _ = tx.send(AppEvent::LeanVerifyStarted);
    let project_dir = resolve_lean_project_dir();

    let session_id = session_snapshot.id.clone();
    let persistent_scratch = store
        .write_scratch(
            &session_id,
            &openproof_lean::render_node_scratch(
                &verification_session,
                verification_session
                    .proof
                    .nodes
                    .iter()
                    .find(|n| {
                        Some(n.id.as_str())
                            == verification_session.proof.active_node_id.as_deref()
                    })
                    .unwrap_or(&verification_session.proof.nodes[0]),
            ),
        )
        .ok()
        .map(|(path, _)| path);

    tokio::spawn(async move {
        let scratch = persistent_scratch.clone();
        let verification_clone = verification_session.clone();
        let result = tokio::task::spawn_blocking(move || {
            openproof_lean::verify_node_at(
                &project_dir,
                &verification_clone,
                verification_clone
                    .proof
                    .nodes
                    .iter()
                    .find(|n| {
                        Some(n.id.as_str())
                            == verification_clone.proof.active_node_id.as_deref()
                    })
                    .unwrap_or(&verification_clone.proof.nodes[0]),
                scratch.as_deref(),
            )
        })
        .await
        .ok()
        .and_then(Result::ok);
        match result {
            Some(result) => {
                let persist_store = store.clone();
                let persist_session = verification_session.clone();
                let persist_result = result.clone();
                let persist_tx = tx.clone();
                tokio::spawn(async move {
                    let persisted = tokio::task::spawn_blocking(move || {
                        persist_store
                            .record_verification_result(&persist_session, &persist_result)
                    })
                    .await
                    .ok()
                    .and_then(Result::ok);
                    if persisted.is_none() {
                        let _ = persist_tx.send(AppEvent::AppendNotice {
                            title: "Verification Store Error".to_string(),
                            content: "Could not persist the branch verification outcome."
                                .to_string(),
                        });
                    }
                });
                let _ = tx.send(AppEvent::BranchVerifyFinished {
                    branch_id,
                    focus_node_id,
                    promote,
                    result,
                });
            }
            None => {
                let _ = tx.send(AppEvent::AppendNotice {
                    title: "Verify Error".to_string(),
                    content: format!("Lean verification crashed for branch {branch_id}."),
                });
            }
        }
    });
}

fn build_branch_verification_session(
    session: &SessionSnapshot,
    branch_id: &str,
) -> Option<(SessionSnapshot, Option<String>)> {
    let branch = session
        .proof
        .branches
        .iter()
        .find(|branch| branch.id == branch_id)?;
    if branch.lean_snippet.trim().is_empty() {
        return None;
    }
    let focus_node_id = branch
        .focus_node_id
        .clone()
        .or_else(|| session.proof.active_node_id.clone())?;
    let mut verification_session = session.clone();
    verification_session.proof.active_node_id = Some(focus_node_id.clone());
    if let Some(node) = verification_session
        .proof
        .nodes
        .iter_mut()
        .find(|node| node.id == focus_node_id)
    {
        node.content = branch.lean_snippet.clone();
    } else {
        return None;
    }
    Some((verification_session, Some(focus_node_id)))
}

pub fn ensure_hidden_agent_branch(
    tx: mpsc::UnboundedSender<AppEvent>,
    store: AppStore,
    state: &mut AppState,
    role: AgentRole,
    title: &str,
    description: &str,
) -> Result<(String, SessionSnapshot), String> {
    let existing_id = state.current_session().and_then(|session| {
        session
            .proof
            .branches
            .iter()
            .filter(|branch| branch.hidden && branch.role == role)
            .max_by(|left, right| left.updated_at.cmp(&right.updated_at))
            .map(|branch| branch.id.clone())
    });

    if let Some(branch_id) = existing_id {
        let now = chrono::Utc::now().to_rfc3339();
        if let Some(session) = state.current_session_mut() {
            if let Some(branch) = session
                .proof
                .branches
                .iter_mut()
                .find(|branch| branch.id == branch_id)
            {
                branch.title = title.to_string();
                branch.hidden = true;
                branch.branch_kind = format!("{}_hidden", agent_role_label(role));
                branch.status = AgentStatus::Running;
                branch.queue_state = BranchQueueState::Running;
                branch.phase = Some(branch_phase_for_role(role).to_string());
                branch.goal_summary = description.to_string();
                branch.search_status = format!("{} branch restarted", agent_role_label(role));
                branch.progress_kind = Some(
                    match role {
                        AgentRole::Planner => "planning",
                        AgentRole::Retriever => "retrieving",
                        AgentRole::Repairer => "repairing",
                        AgentRole::Prover => "candidate",
                        AgentRole::Critic => "blocked",
                    }
                    .to_string(),
                );
                branch.summary = description.to_string();
                branch.updated_at = now.clone();
            }
            session.updated_at = now;
        }
        persist_current_session(
            tx,
            store,
            state,
            format!("Restarted {} branch.", agent_role_label(role)),
        );
        let snapshot = state
            .current_session()
            .cloned()
            .ok_or_else(|| "No active session.".to_string())?;
        return Ok((branch_id, snapshot));
    }

    let (write, branch_id, _task_id) =
        state.spawn_agent_branch(role, title, description, true)?;
    let snapshot = write.session.clone();
    persist_write(tx, store, write);
    Ok((branch_id, snapshot))
}

pub fn submit_selected_question_option(
    tx: mpsc::UnboundedSender<AppEvent>,
    store: AppStore,
    state: &mut AppState,
) {
    let Some(option) = state.selected_question_option().cloned() else {
        emit_local_notice(
            tx,
            state,
            store,
            "Answer Error",
            "No clarification option is currently selectable.".to_string(),
        );
        return;
    };
    let reply_text = if option.formal_target.trim().is_empty() {
        option.label.clone()
    } else {
        option.formal_target.clone()
    };
    if let Some(submitted) = state.submit_text(reply_text) {
        persist_write(
            tx.clone(),
            store.clone(),
            PendingWrite {
                session: submitted.session_snapshot.clone(),
            },
        );
        handle_submission(tx, store, state, submitted);
    }
}

pub fn persist_verification_result(
    tx: mpsc::UnboundedSender<AppEvent>,
    store: AppStore,
    session: SessionSnapshot,
    result: openproof_protocol::LeanVerificationSummary,
) {
    tokio::spawn(async move {
        let outcome =
            tokio::task::spawn_blocking(move || store.record_verification_result(&session, &result))
                .await
                .ok()
                .and_then(Result::ok);
        if outcome.is_none() {
            let _ = tx.send(AppEvent::AppendNotice {
                title: "Verification Store Error".to_string(),
                content: "Could not persist the verification outcome into the native corpus store."
                    .to_string(),
            });
        }
    });
}
