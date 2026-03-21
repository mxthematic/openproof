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
                    persist_verification_result(tx.clone(), store.clone(), session, result);
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
        KeyCode::Up if state.has_open_question() => Some(AppEvent::SelectPrevQuestionOption),
        KeyCode::Down if state.has_open_question() => Some(AppEvent::SelectNextQuestionOption),
        KeyCode::Up => Some(match state.focus {
            FocusPane::Sessions => AppEvent::SelectPrevSession,
            _ => AppEvent::ScrollTranscriptUp,
        }),
        KeyCode::Down => Some(match state.focus {
            FocusPane::Sessions => AppEvent::SelectNextSession,
            _ => AppEvent::ScrollTranscriptDown,
        }),
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
