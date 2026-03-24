//! Main TUI event loop (`run_app`).
//!
//! Drives the render/input cycle: drains the async `AppEvent` channel,
//! flushes completed transcript turns to terminal scrollback, renders the
//! frame, and dispatches keyboard/mouse/paste events.

use crate::autonomous::schedule_autonomous_tick;
use crate::helpers::{
    best_hidden_branch, current_foreground_branch, persist_write, should_promote_hidden_branch,
};
use crate::key_handling::{handle_command_mode_key, handle_overlay_key};
use crate::turn_handling::{
    handle_submission, persist_verification_result, start_branch_verification,
    submit_selected_question_option,
};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use openproof_core::{AppEvent, AppState, FocusPane, PendingWrite};
use openproof_store::AppStore;
use ratatui::backend::CrosstermBackend;
use std::{io, io::Write as _, time::Duration};
use tokio::sync::mpsc;

pub async fn run_app(
    terminal: &mut openproof_tui::custom_terminal::CustomTerminal<CrosstermBackend<io::Stdout>>,
    store: AppStore,
    state: &mut AppState,
    tx: mpsc::UnboundedSender<AppEvent>,
    rx: &mut mpsc::UnboundedReceiver<AppEvent>,
) -> anyhow::Result<()> {
    let mut last_session_id = state
        .current_session()
        .map(|s| s.id.clone())
        .unwrap_or_default();

    loop {
        // Detect session change -- clear scrollback and reset viewport.
        let current_session_id = state
            .current_session()
            .map(|s| s.id.clone())
            .unwrap_or_default();
        if current_session_id != last_session_id {
            last_session_id = current_session_id;
            let size = terminal.size()?;
            terminal.set_viewport_area(ratatui::layout::Rect::new(
                0, 0, size.width, size.height,
            ));
            let writer = terminal.backend_mut();
            write!(writer, "\x1b[r\x1b[0m\x1b[H\x1b[2J\x1b[3J\x1b[H")?;
            io::Write::flush(writer)?;
            terminal.clear()?;
        }

        while let Ok(event) = rx.try_recv() {
            if matches!(event, AppEvent::AutonomousTick) {
                schedule_autonomous_tick(tx.clone(), store.clone(), state);
                continue;
            }
            let verification_result = match &event {
                AppEvent::LeanVerifyFinished(result) => Some(result.clone()),
                _ => None,
            };
            let branch_verification = match &event {
                AppEvent::BranchVerifyFinished {
                    branch_id,
                    focus_node_id,
                    promote,
                    result,
                } => Some((
                    branch_id.clone(),
                    focus_node_id.clone(),
                    *promote,
                    result.clone(),
                )),
                _ => None,
            };
            let is_assistant_append = matches!(&event, AppEvent::AppendAssistant(_));
            let finished_branch_id = match &event {
                AppEvent::FinishBranch { branch_id, .. } => Some(branch_id.clone()),
                _ => None,
            };
            if let Some(write) = state.apply(event.clone()) {
                let verification_session = verification_result
                    .as_ref()
                    .map(|_| write.session.clone());
                persist_write(tx.clone(), store.clone(), write);
                if let (Some(result), Some(session)) = (verification_result, verification_session) {
                    let verified_ok = result.ok;
                    persist_verification_result(tx.clone(), store.clone(), session.clone(), result);

                    // Auto-sync to cloud after successful verification
                    if verified_ok
                        && session.cloud.sync_enabled
                        && session.cloud.share_mode != openproof_protocol::ShareMode::Local
                    {
                        let store_sync = store.clone();
                        let share_mode = session.cloud.share_mode;
                        tokio::spawn(async move {
                            let corpus = openproof_corpus::CorpusManager::new(
                                store_sync,
                                openproof_cloud::CloudCorpusClient::new(Default::default()),
                                std::path::PathBuf::from("."),
                            );
                            let _ = corpus.drain_sync_queue(share_mode, true, None).await;
                        });
                    }
                }
            }
            if let Some((branch_id, _focus_node_id, _promote, _result)) = branch_verification {
                if state
                    .current_session()
                    .map(|session| session.proof.is_autonomous_running)
                    .unwrap_or(false)
                {
                    let _ = tx.send(AppEvent::AutonomousTick);
                }
                if let Some(branch) = state
                    .current_session()
                    .and_then(|session| {
                        session
                            .proof
                            .branches
                            .iter()
                            .find(|branch| branch.id == branch_id)
                    })
                {
                    if branch.hidden
                        && should_promote_hidden_branch(
                            state
                                .current_session()
                                .and_then(|session| best_hidden_branch(session).cloned()),
                            current_foreground_branch(state.current_session()).cloned(),
                        )
                    {
                        if let Some(candidate_id) = state
                            .current_session()
                            .and_then(|session| {
                                best_hidden_branch(session).map(|branch| branch.id.clone())
                            })
                        {
                            if let Ok(write) =
                                state.promote_branch_to_foreground(&candidate_id, false, None)
                            {
                                persist_write(tx.clone(), store.clone(), write);
                            }
                        }
                    }
                }
            }
            if let Some(branch_id) = finished_branch_id {
                if let Some(session_snapshot) = state.current_session().cloned() {
                    if let Some((branch_id, hidden)) = session_snapshot
                        .proof
                        .branches
                        .iter()
                        .find(|branch| branch.id == branch_id)
                        .map(|branch| (branch.id.clone(), branch.hidden))
                    {
                        if session_snapshot
                            .proof
                            .branches
                            .iter()
                            .find(|branch| branch.id == branch_id)
                            .map(|branch| !branch.lean_snippet.trim().is_empty())
                            .unwrap_or(false)
                        {
                            start_branch_verification(
                                tx.clone(),
                                store.clone(),
                                session_snapshot,
                                branch_id.clone(),
                                !hidden,
                            );
                        } else if state
                            .current_session()
                            .map(|session| session.proof.is_autonomous_running)
                            .unwrap_or(false)
                        {
                            let _ = tx.send(AppEvent::AutonomousTick);
                        }
                    }
                }
            }

            // After AppendAssistant or TurnFinished: ensure nodes exist and workspace is synced.
            // This mirrors what the headless runner does but was missing in TUI mode.
            let is_turn_finished = matches!(&event, AppEvent::TurnFinished);
            if is_turn_finished || is_assistant_append {
                if let Some(session) = state.current_session().cloned() {
                    // Create a node if none exist (headless fallback equivalent)
                    if session.proof.nodes.is_empty() {
                        eprintln!("[tui-sync] Creating node (nodes empty, finished={is_turn_finished}, assistant={is_assistant_append})");
                        let label = if session.title != "OpenProof Rust Session" && !session.title.trim().is_empty() {
                            session.title.clone()
                        } else {
                            session.proof.accepted_target.as_ref()
                                .or(session.proof.formal_target.as_ref())
                                .cloned()
                                .unwrap_or_else(|| "Goal".to_string())
                        };
                        let statement = session.proof.accepted_target.as_ref()
                            .or(session.proof.formal_target.as_ref())
                            .or(session.proof.problem.as_ref())
                            .cloned()
                            .unwrap_or_else(|| label.clone());
                        if let Ok(write) = state.add_proof_node(
                            openproof_protocol::ProofNodeKind::Theorem,
                            &label,
                            &statement,
                        ) {
                            persist_write(tx.clone(), store.clone(), write);
                        }
                    }

                    // Sync workspace .lean files to node content
                    let ws_dir = store.workspace_dir(&session.id);
                    let mut all_lean = String::new();
                    if let Ok(files) = store.list_workspace_files(&session.id) {
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
                    if !all_lean.trim().is_empty() {
                        if let Some(s) = state.current_session_mut() {
                            // Parse Lean declarations and rebuild nodes from the actual code.
                            // Each theorem/lemma/def becomes a node. This keeps nodes
                            // in sync with what's actually in the workspace files.
                            let parsed = openproof_lean::parse_lean_declarations(&all_lean);
                            if !parsed.is_empty() {
                                let now = chrono::Utc::now().to_rfc3339();
                                let parsed_nodes = openproof_lean::declarations_to_proof_nodes(
                                    &parsed, &s.id,
                                );
                                // Preserve status of existing nodes by label
                                let old_statuses: std::collections::HashMap<String, openproof_protocol::ProofNodeStatus> =
                                    s.proof.nodes.iter().map(|n| (n.label.clone(), n.status)).collect();
                                let active_label = s.proof.active_node_id.as_deref()
                                    .and_then(|id| s.proof.nodes.iter().find(|n| n.id == id))
                                    .map(|n| n.label.clone());

                                s.proof.nodes = parsed_nodes.iter().map(|pn| {
                                    let mut node = pn.clone();
                                    // Preserve old status unless it was stale
                                    if let Some(&prev) = old_statuses.get(&node.label) {
                                        if prev != openproof_protocol::ProofNodeStatus::Pending {
                                            node.status = prev;
                                        }
                                    }
                                    // Mark sorry-containing nodes as Proving
                                    if node.content.contains("sorry") {
                                        node.status = openproof_protocol::ProofNodeStatus::Proving;
                                    }
                                    node.updated_at = now.clone();
                                    node
                                }).collect();

                                // Restore active node by label
                                if let Some(label) = &active_label {
                                    s.proof.active_node_id = s.proof.nodes.iter()
                                        .find(|n| &n.label == label).map(|n| n.id.clone());
                                }
                                if s.proof.active_node_id.is_none() {
                                    s.proof.active_node_id = s.proof.nodes.first().map(|n| n.id.clone());
                                }
                                if let Some(root) = s.proof.nodes.first() {
                                    s.proof.root_node_id = Some(root.id.clone());
                                }
                            }

                            // Also store the full workspace content on the active node
                            if let Some(node_id) = s.proof.active_node_id.clone() {
                                if let Some(node) = s.proof.nodes.iter_mut().find(|n| n.id == node_id) {
                                    if node.content.trim().is_empty() {
                                        node.content = all_lean.clone();
                                    }
                                }
                            }
                            s.proof.last_rendered_scratch = Some(all_lean.clone());
                            // Extract title from workspace file comments or LaTeX \title{}
                            if s.title == "OpenProof Rust Session" || s.title.trim().is_empty() {
                                // Try TITLE: marker in Lean comments
                                for line in all_lean.lines() {
                                    let trimmed = line.trim();
                                    if let Some(title) = trimmed.strip_prefix("TITLE:").or_else(|| trimmed.strip_prefix("TITLE :")) {
                                        let title = title.trim();
                                        if !title.is_empty() {
                                            s.title = title.to_string();
                                            break;
                                        }
                                    }
                                }
                                // Also try extracting from LaTeX \title{...} in paper_tex
                                if s.title == "OpenProof Rust Session" || s.title.trim().is_empty() {
                                    if let Some(start) = s.proof.paper_tex.find("\\title{") {
                                        let after = &s.proof.paper_tex[start + 7..];
                                        if let Some(end) = after.find('}') {
                                            let title = after[..end].trim();
                                            if !title.is_empty() {
                                                s.title = title.replace("\\(", "").replace("\\)", "").to_string();
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        if let Some(session) = state.current_session().cloned() {
                            let s = store.clone();
                            tokio::spawn(async move {
                                let _ = tokio::task::spawn_blocking(move || s.save_session(&session)).await;
                            });
                        }
                    }
                }
            }

            // After AppendAssistant: if lean code was extracted, auto-verify it.
            if is_assistant_append && !state.verification_in_flight {
                // Extract what we need before mutating state.
                let verify_info = state.current_session().and_then(|s| {
                    let node = s.proof.active_node_id.as_deref()
                        .and_then(|id| s.proof.nodes.iter().find(|n| n.id == id))?;
                    if node.status == openproof_protocol::ProofNodeStatus::Proving
                        && !node.content.trim().is_empty()
                    {
                        Some((s.id.clone(), node.content.clone(), s.proof.imports.clone()))
                    } else {
                        None
                    }
                });
                if let Some((session_id, node_content, imports)) = verify_info {
                    let _ = store.write_scratch(&session_id, &node_content);
                    let _ = state.apply(AppEvent::LeanVerifyStarted);
                    let tx_v = tx.clone();
                    let lean_dir = crate::helpers::resolve_lean_project_dir();
                    tokio::spawn(async move {
                        let result = tokio::task::spawn_blocking(move || {
                            openproof_lean::verify_scratch_content(
                                &lean_dir, &node_content, None, &imports,
                            )
                        })
                        .await
                        .ok()
                        .and_then(|r| r.ok());
                        let summary = result.unwrap_or_else(|| {
                            openproof_protocol::LeanVerificationSummary {
                                ok: false,
                                error: Some("Lean verification failed to run".to_string()),
                                ..Default::default()
                            }
                        });
                        let _ = tx_v.send(AppEvent::LeanVerifyFinished(summary));
                    });
                }
            }
        }

        // Flush completed turns to terminal scrollback (enables native scrollbar).
        if !state.turn_in_flight {
            if let Some(session) = state.current_session() {
                let transcript_len = session.transcript.len();
                let flushable = transcript_len.saturating_sub(1);
                if flushable > state.flushed_turn_count {
                    let entries_to_flush: Vec<_> = session.transcript
                        [state.flushed_turn_count..flushable]
                        .to_vec();
                    let mut lines = Vec::new();
                    for entry in &entries_to_flush {
                        lines.extend(openproof_tui::render_entry(entry));
                    }
                    if !lines.is_empty() {
                        let _ = openproof_tui::insert_history::insert_history_lines(
                            terminal, lines,
                        );
                    }
                    state.flushed_turn_count = flushable;
                }
            }
        }

        terminal.draw(|frame| openproof_tui::draw(frame, state))?;

        if state.should_quit {
            break;
        }

        // Drain all pending terminal events before rendering.
        let mut poll_timeout = Duration::from_millis(16);
        while event::poll(poll_timeout)? {
            poll_timeout = Duration::ZERO;
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if state.overlay.is_some() {
                        handle_overlay_key(key, state, &tx, &store);
                    } else if state.command_mode {
                        handle_command_mode_key(key, state, &tx, &store);
                    } else {
                        handle_normal_mode_key(key, state, &tx, &store);
                    }
                }
                Event::Paste(text) => {
                    if let Some(write) = state.apply(AppEvent::Paste(text)) {
                        persist_write(tx.clone(), store.clone(), write);
                    }
                }
                Event::Mouse(mouse) => {
                    use crossterm::event::MouseEventKind;
                    match mouse.kind {
                        MouseEventKind::ScrollUp => {
                            let _ = state.apply(AppEvent::ScrollTranscriptUp);
                        }
                        MouseEventKind::ScrollDown => {
                            let _ = state.apply(AppEvent::ScrollTranscriptDown);
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn handle_normal_mode_key(
    key: event::KeyEvent,
    state: &mut AppState,
    tx: &mpsc::UnboundedSender<AppEvent>,
    store: &AppStore,
) {
    let next_event = match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if !state.composer.is_empty() {
                state.composer.clear();
                state.composer_cursor = 0;
                None
            } else {
                Some(AppEvent::Quit)
            }
        }
        KeyCode::Tab => Some(AppEvent::FocusNext),
        KeyCode::BackTab => {
            // Shift+Tab: cycle autonomous mode (off -> normal -> full -> off)
            if let Some(session) = state.current_session_mut() {
                let (new_running, new_full, label) = match (session.proof.is_autonomous_running, session.proof.full_autonomous) {
                    (false, _) => (true, false, "autonomous on"),
                    (true, false) => (true, true, "full autonomous on"),
                    (true, true) => (false, false, "autonomous off"),
                };
                session.proof.is_autonomous_running = new_running;
                session.proof.full_autonomous = new_full;
                state.status = format!("{label} (shift+tab to cycle)");
                if new_running {
                    Some(AppEvent::AutonomousTick)
                } else {
                    None
                }
            } else {
                None
            }
        }
        KeyCode::Esc if state.turn_in_flight => {
            // Abort current turn.
            state.turn_in_flight = false;
            state.turn_started_at = None;
            state.streaming_text.clear();
            state.status = "Aborted.".to_string();
            None
        }
        KeyCode::Esc if !state.composer.is_empty() => {
            // Clear composer.
            state.composer.clear();
            state.composer_cursor = 0;
            state.history_index = None;
            state.input_draft.clear();
            None
        }
        KeyCode::Up if state.has_open_question() => Some(AppEvent::SelectPrevQuestionOption),
        KeyCode::Down if state.has_open_question() => Some(AppEvent::SelectNextQuestionOption),
        KeyCode::Up => {
            if !state.input_history.is_empty() {
                // Browse input history backward (shell-style).
                if state.history_index.is_none() {
                    state.input_draft = state.composer.clone();
                }
                let idx = match state.history_index {
                    Some(0) => 0,
                    Some(i) => i - 1,
                    None => state.input_history.len() - 1,
                };
                state.history_index = Some(idx);
                state.composer = state.input_history[idx].clone();
                state.composer_cursor = state.composer.len();
                None
            } else {
                Some(AppEvent::ScrollTranscriptUp)
            }
        }
        KeyCode::Down => {
            if state.history_index.is_some() {
                // Browse input history forward.
                match state.history_index {
                    Some(i) if i + 1 < state.input_history.len() => {
                        state.history_index = Some(i + 1);
                        state.composer = state.input_history[i + 1].clone();
                        state.composer_cursor = state.composer.len();
                    }
                    Some(_) => {
                        state.history_index = None;
                        state.composer = state.input_draft.clone();
                        state.composer_cursor = state.composer.len();
                        state.input_draft.clear();
                    }
                    None => {}
                }
                None
            } else {
                Some(AppEvent::ScrollTranscriptDown)
            }
        }
        KeyCode::PageUp => Some(AppEvent::ScrollPageUp),
        KeyCode::PageDown => Some(AppEvent::ScrollPageDown),
        KeyCode::Left => Some(AppEvent::CursorLeft),
        KeyCode::Right => Some(AppEvent::CursorRight),
        KeyCode::Home => Some(AppEvent::CursorHome),
        KeyCode::End => Some(AppEvent::CursorEnd),
        KeyCode::Delete => Some(AppEvent::DeleteForward),
        KeyCode::Backspace => Some(AppEvent::Backspace),
        KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(AppEvent::CursorHome)
        }
        KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(AppEvent::CursorEnd)
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(AppEvent::ClearToStart)
        }
        KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(AppEvent::DeleteWordBackward)
        }
        KeyCode::Char('/')
            if state.composer.is_empty()
                && !key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            state.command_mode = true;
            state.command_buffer.clear();
            state.command_cursor = 0;
            state.command_completions = openproof_core::command_completions("");
            state.completion_idx = None;
            None
        }
        KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(AppEvent::InputChar(ch))
        }
        _ => None,
    };

    if let Some(next_event) = next_event {
        if let Some(write) = state.apply(next_event) {
            persist_write(tx.clone(), store.clone(), write);
        }
    } else if matches!(key.code, KeyCode::Enter) {
        if state.has_open_question() && state.composer.trim().is_empty() {
            submit_selected_question_option(tx.clone(), store.clone(), state);
        } else if let Some(submission) = state.submit_composer() {
            persist_write(
                tx.clone(),
                store.clone(),
                PendingWrite {
                    session: submission.session_snapshot.clone(),
                },
            );
            handle_submission(tx.clone(), store.clone(), state, submission);
        }
    }
}
