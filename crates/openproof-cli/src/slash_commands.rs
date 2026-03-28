//! Slash-command dispatch (`/help`, `/new`, `/autonomous`, etc.).
//!
//! All `/command` strings entered by the user flow through
//! `apply_local_command`.  Each command arm is self-contained: it reads from
//! `state`, fires off any async work via `tx`, and produces notices via
//! `emit_local_notice`.
//!
//! Large sub-command handlers live in sibling modules:
//! - `slash_share_corpus`: `/share`, `/corpus`, `/sync`
//! - `slash_autonomous`: `/autonomous`, `/answer`, `/theorem`, `/lemma`, `/verify`

use crate::export::{append_memory_entry, export_session_artifacts};
use crate::helpers::{
    emit_local_notice, parse_agent_role, persist_write, resolve_lean_project_dir,
};
use crate::slash_autonomous::{
    apply_statement_command, cmd_answer, cmd_autonomous, start_verify_active_node,
};
use crate::slash_share_corpus::{cmd_corpus, cmd_share, cmd_sync};
use crate::turn_handling::start_agent_branch_turn;
use openproof_core::{AppEvent, AppState, SubmittedInput};
use openproof_protocol::ProofNodeKind;
use openproof_store::AppStore;
use std::time::Duration;
use tokio::sync::mpsc;

pub fn apply_local_command(
    tx: mpsc::UnboundedSender<AppEvent>,
    state: &mut AppState,
    store: AppStore,
    submission: SubmittedInput,
) {
    let trimmed = submission.raw_text.trim();
    let mut parts = trimmed.splitn(2, ' ');
    let command = parts.next().unwrap_or("");
    let arg_text = parts.next().unwrap_or("").trim();
    match command {
        "/help" => {
            emit_local_notice(
                tx,
                state,
                store,
                "Help",
                [
                    "/help",
                    "/new <title>",
                    "/resume <session-id>",
                    "/nodes",
                    "/focus <branch-id|node-id|clear>",
                    "/agent spawn <role> <task>",
                    "/proof",
                    "/lean",
                    "/paper",
                    "/answer <option-id|text>",
                    "/memory",
                    "/remember <text>",
                    "/remember global <text>",
                    "/share [local|community|private]",
                    "/share overlay [on|off]",
                    "/corpus status|search <query>|ingest|recluster",
                    "/sync status|enable|disable|drain",
                    "/export paper|tex|lean|all",
                    "/autonomous status|start|full|stop|step",
                    "/theorem <label> :: <statement>",
                    "/lemma <label> :: <statement>",
                    "/verify",
                    "/dashboard",
                    "Tab focuses panes. Enter sends. Ctrl+C quits.",
                ]
                .join("\n"),
            );
        }
        "/new" => {
            let write = state.create_session(if arg_text.is_empty() {
                None
            } else {
                Some(arg_text)
            });
            persist_write(tx.clone(), store.clone(), write);
            emit_local_notice(
                tx,
                state,
                store,
                "Session",
                format!(
                    "Started new session: {}.",
                    state
                        .current_session()
                        .map(|session| session.title.clone())
                        .unwrap_or_else(|| "OpenProof Rust Session".to_string())
                ),
            );
        }
        "/resume" => {
            if arg_text.is_empty() {
                state.overlay = Some(openproof_core::Overlay::SessionPicker {
                    selected: state.selected_session,
                });
            } else {
                match state.switch_session(arg_text) {
                    Ok(()) => emit_local_notice(
                        tx,
                        state,
                        store,
                        "Session",
                        format!(
                            "Resumed {}.",
                            state
                                .current_session()
                                .map(|session| session.title.clone())
                                .unwrap_or_else(|| arg_text.to_string())
                        ),
                    ),
                    Err(error) => emit_local_notice(tx, state, store, "Session Error", error),
                }
            }
        }
        "/nodes" => {
            emit_local_notice(tx, state, store, "Proof Nodes", state.proof_nodes_report());
        }
        "/focus" => {
            if arg_text.is_empty() {
                let items = openproof_core::build_focus_items(state);
                if items.is_empty() {
                    emit_local_notice(
                        tx,
                        state,
                        store,
                        "Focus",
                        "No focusable targets (no nodes or branches).".to_string(),
                    );
                } else {
                    state.overlay =
                        Some(openproof_core::Overlay::FocusPicker { items, selected: 0 });
                }
            } else if arg_text == "clear" {
                match state.focus_target(None) {
                    Ok(Some(write)) => {
                        persist_write(tx.clone(), store.clone(), write);
                        emit_local_notice(
                            tx,
                            state,
                            store,
                            "Focus",
                            "Cleared active proof focus.".to_string(),
                        );
                    }
                    Ok(None) => {}
                    Err(error) => emit_local_notice(tx, state, store, "Focus Error", error),
                }
            } else {
                match state.focus_target(Some(arg_text)) {
                    Ok(Some(write)) => {
                        persist_write(tx.clone(), store.clone(), write);
                        emit_local_notice(
                            tx,
                            state,
                            store,
                            "Focus",
                            format!("Focused {arg_text}."),
                        );
                    }
                    Ok(None) => {}
                    Err(error) => emit_local_notice(tx, state, store, "Focus Error", error),
                }
            }
        }
        "/agent" => {
            let parts = arg_text.split_whitespace().collect::<Vec<_>>();
            if parts.first().copied() != Some("spawn") || parts.len() < 3 {
                emit_local_notice(
                    tx,
                    state,
                    store,
                    "Agent Usage",
                    "Usage: /agent spawn <planner|prover|repairer|retriever|critic> <task>"
                        .to_string(),
                );
                return;
            }
            let Some(role) = parse_agent_role(parts[1]) else {
                emit_local_notice(
                    tx,
                    state,
                    store,
                    "Agent Usage",
                    "Unknown agent role. Use planner|prover|repairer|retriever|critic.".to_string(),
                );
                return;
            };
            let title = parts[2..].join(" ");
            match state.spawn_agent_branch(role, &title, &title, false) {
                Ok((write, branch_id, task_id)) => {
                    let session_snapshot = write.session.clone();
                    persist_write(tx.clone(), store.clone(), write);
                    start_agent_branch_turn(
                        tx.clone(),
                        store.clone(),
                        role,
                        title.clone(),
                        branch_id.clone(),
                        task_id.clone(),
                        session_snapshot,
                    );
                    emit_local_notice(
                        tx,
                        state,
                        store,
                        "Agent",
                        format!(
                            "Started {} branch {} with task {}.",
                            crate::helpers::agent_role_label(role),
                            branch_id,
                            task_id
                        ),
                    );
                }
                Err(error) => emit_local_notice(tx, state, store, "Agent Error", error),
            }
        }
        "/proof" => {
            let report = state.proof_status_report();
            emit_local_notice(tx, state, store, "Proof State", report);
        }
        "/lean" => {
            let session = state.current_session().cloned();
            let content = if let Some(session) = &session {
                let scratch = store.read_scratch(&session.id);
                let history = store.list_scratch_history(&session.id);
                let scratch_path = session.proof.scratch_path.as_deref().unwrap_or("(not set)");
                let attempt = session.proof.attempt_number;
                let verification = session.proof.last_verification.as_ref();
                let mut lines = vec![
                    format!("Scratch: {scratch_path}"),
                    format!("Attempts: {attempt}"),
                    format!("History: {} files", history.len()),
                ];
                if let Some(v) = verification {
                    lines.push(format!(
                        "Last check: {}",
                        if v.ok { "OK" } else { "FAILED" }
                    ));
                    if !v.ok {
                        for line in v.stderr.lines().take(3) {
                            lines.push(format!("  {line}"));
                        }
                    }
                }
                if let Some(content) = scratch {
                    lines.push(String::new());
                    lines.push("```lean".to_string());
                    lines.push(content);
                    lines.push("```".to_string());
                } else {
                    lines.push("No Scratch.lean file yet.".to_string());
                }
                lines.join("\n")
            } else {
                "No active session.".to_string()
            };
            emit_local_notice(tx, state, store, "Lean State", content);
        }
        "/paper" => {
            if let Some(session) = state.current_session().cloned() {
                if !session.proof.paper_tex.trim().is_empty() {
                    if let Ok(path) = store.write_paper(&session.id, &session.proof.paper_tex) {
                        emit_local_notice(
                            tx.clone(),
                            state,
                            store.clone(),
                            "Paper",
                            format!("{}\n\nWritten to: {}", state.paper_report(), path.display()),
                        );
                    } else {
                        emit_local_notice(tx, state, store, "Paper", state.paper_report());
                    }
                } else {
                    emit_local_notice(tx, state, store, "Paper", state.paper_report());
                }
            } else {
                emit_local_notice(tx, state, store, "Paper", "No active session.".to_string());
            }
        }
        "/memory" => {
            let context = crate::system_prompt::load_prompt_context();
            let content = if context.memory.trim().is_empty() {
                "No memory recorded yet.".to_string()
            } else {
                context.memory
            };
            emit_local_notice(tx, state, store, "Memory", content);
        }
        "/remember" => {
            if arg_text.is_empty() {
                emit_local_notice(
                    tx,
                    state,
                    store,
                    "Remember Usage",
                    "Usage: /remember <text> or /remember global <text>".to_string(),
                );
                return;
            }
            let context = crate::system_prompt::load_prompt_context();
            let (target_path, text) = if let Some(rest) = arg_text.strip_prefix("global ") {
                (context.global_memory_path, rest.trim())
            } else {
                (context.workspace_memory_path, arg_text.trim())
            };
            if text.is_empty() {
                emit_local_notice(
                    tx,
                    state,
                    store,
                    "Remember Usage",
                    "Usage: /remember <text> or /remember global <text>".to_string(),
                );
                return;
            }
            match append_memory_entry(&target_path, text) {
                Ok(line) => emit_local_notice(tx, state, store, "Memory Saved", line),
                Err(error) => {
                    emit_local_notice(tx, state, store, "Memory Error", error.to_string())
                }
            }
        }
        "/share" => {
            cmd_share(tx, state, store, arg_text);
        }
        "/corpus" => {
            cmd_corpus(tx, state, store, arg_text);
        }
        "/sync" => {
            cmd_sync(tx, state, store, arg_text);
        }
        "/export" => {
            let target = if arg_text.is_empty() { "all" } else { arg_text };
            match export_session_artifacts(state.current_session(), target) {
                Ok(paths) => emit_local_notice(
                    tx,
                    state,
                    store,
                    "Export",
                    if paths.is_empty() {
                        "Nothing was exported.".to_string()
                    } else {
                        paths.join("\n")
                    },
                ),
                Err(error) => {
                    emit_local_notice(tx, state, store, "Export Error", error.to_string())
                }
            }
        }
        "/autonomous" => {
            cmd_autonomous(tx, state, store, arg_text);
        }
        "/answer" => {
            cmd_answer(tx, state, store, arg_text);
        }
        "/theorem" => apply_statement_command(tx, state, store, ProofNodeKind::Theorem, arg_text),
        "/lemma" => apply_statement_command(tx, state, store, ProofNodeKind::Lemma, arg_text),
        "/verify" => start_verify_active_node(tx, state, store),
        "/dashboard" => {
            let store_dash = store.clone();
            let tx_dash = tx.clone();
            let lean_dir = resolve_lean_project_dir();
            tokio::spawn(async move {
                match openproof_dashboard::start_dashboard_server(store_dash, lean_dir, None).await
                {
                    Ok(server) => {
                        let url = format!("http://127.0.0.1:{}", server.port);
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        openproof_dashboard::open_browser(&url);
                        let _ = tx_dash.send(AppEvent::AppendNotice {
                            title: "Dashboard".to_string(),
                            content: format!("Dashboard opened at {url}"),
                        });
                        std::mem::forget(server);
                    }
                    Err(e) => {
                        let _ = tx_dash.send(AppEvent::AppendNotice {
                            title: "Dashboard Error".to_string(),
                            content: format!("Could not start dashboard: {e}"),
                        });
                    }
                }
            });
        }
        _ => {
            emit_local_notice(
                tx,
                state,
                store,
                "Unknown Command",
                format!("Unknown local command: {trimmed}"),
            );
        }
    }
}
