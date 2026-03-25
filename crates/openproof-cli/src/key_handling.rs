//! Keyboard event handlers for the TUI.
//!
//! Covers the overlay picker key handler and the command-mode key handler.
//! Normal-mode key dispatch lives inline in `event_loop::run_app` since it
//! needs direct access to the event-loop locals.

use crate::helpers::{emit_local_notice, persist_write};
use crossterm::event::{self, KeyCode, KeyModifiers};
use openproof_core::AppState;
use openproof_store::AppStore;
use tokio::sync::mpsc;
use openproof_core::AppEvent;

pub fn handle_overlay_key(
    key: event::KeyEvent,
    state: &mut AppState,
    tx: &mpsc::UnboundedSender<AppEvent>,
    store: &AppStore,
) {
    let Some(overlay) = state.overlay.take() else {
        return;
    };
    match overlay {
        openproof_core::Overlay::SessionPicker { mut selected } => match key.code {
            KeyCode::Esc => {
                // Close without action.
            }
            KeyCode::Up => {
                selected = selected.saturating_sub(1);
                state.overlay = Some(openproof_core::Overlay::SessionPicker { selected });
            }
            KeyCode::Down => {
                if selected + 1 < state.sessions.len() {
                    selected += 1;
                }
                state.overlay = Some(openproof_core::Overlay::SessionPicker { selected });
            }
            KeyCode::Enter => {
                if let Some(session) = state.sessions.get(selected) {
                    let id = session.id.clone();
                    match state.switch_session(&id) {
                        Ok(()) => {
                            state.sync_question_selection();
                        }
                        Err(e) => {
                            emit_local_notice(
                                tx.clone(),
                                state,
                                store.clone(),
                                "Resume Error",
                                e,
                            );
                        }
                    }
                }
            }
            _ => {
                // Keep overlay open on unrecognized keys.
                state.overlay = Some(openproof_core::Overlay::SessionPicker { selected });
            }
        },
        openproof_core::Overlay::FocusPicker { items, mut selected } => match key.code {
            KeyCode::Esc => {
                // Close without action.
            }
            KeyCode::Up => {
                selected = selected.saturating_sub(1);
                state.overlay =
                    Some(openproof_core::Overlay::FocusPicker { items, selected });
            }
            KeyCode::Down => {
                if selected + 1 < items.len() {
                    selected += 1;
                }
                state.overlay =
                    Some(openproof_core::Overlay::FocusPicker { items, selected });
            }
            KeyCode::Enter => {
                if let Some((id, _label, _kind)) = items.get(selected) {
                    match state.focus_target(Some(id)) {
                        Ok(Some(write)) => {
                            persist_write(tx.clone(), store.clone(), write);
                            emit_local_notice(
                                tx.clone(),
                                state,
                                store.clone(),
                                "Focus",
                                format!("Focused {id}."),
                            );
                        }
                        Ok(None) => {}
                        Err(e) => {
                            emit_local_notice(
                                tx.clone(),
                                state,
                                store.clone(),
                                "Focus Error",
                                e,
                            );
                        }
                    }
                }
            }
            _ => {
                state.overlay =
                    Some(openproof_core::Overlay::FocusPicker { items, selected });
            }
        },
    }
}

pub fn handle_command_mode_key(
    key: event::KeyEvent,
    state: &mut AppState,
    tx: &mpsc::UnboundedSender<AppEvent>,
    store: &AppStore,
    prover: &Option<openproof_lean::proof_tree::SharedProver>,
) {
    use crate::turn_handling::handle_submission;

    match key.code {
        KeyCode::Esc => {
            state.command_mode = false;
            state.command_buffer.clear();
            state.command_cursor = 0;
            state.command_completions.clear();
            state.completion_idx = None;
        }
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.command_mode = false;
            state.command_buffer.clear();
            state.command_cursor = 0;
            state.command_completions.clear();
            state.completion_idx = None;
        }
        KeyCode::Enter => {
            let buffer = state.command_buffer.clone();
            state.command_mode = false;
            state.command_buffer.clear();
            state.command_cursor = 0;
            state.command_completions.clear();
            state.completion_idx = None;
            if !buffer.is_empty() {
                let text = format!("/{buffer}");
                if let Some(submission) = state.submit_text(text) {
                    persist_write(
                        tx.clone(),
                        store.clone(),
                        openproof_core::PendingWrite {
                            session: submission.session_snapshot.clone(),
                        },
                    );
                    handle_submission(tx.clone(), store.clone(), state, submission, prover.clone());
                }
            }
        }
        KeyCode::Tab => {
            // Cycle to next completion.
            if state.command_completions.is_empty() {
                state.command_completions =
                    openproof_core::command_completions(&state.command_buffer);
                state.completion_idx = None;
            }
            if !state.command_completions.is_empty() {
                let idx = match state.completion_idx {
                    Some(i) => (i + 1) % state.command_completions.len(),
                    None => 0,
                };
                state.completion_idx = Some(idx);
                state.command_buffer = state.command_completions[idx].clone();
                state.command_cursor = state.command_buffer.len();
            }
        }
        KeyCode::BackTab => {
            // Cycle to previous completion.
            if state.command_completions.is_empty() {
                state.command_completions =
                    openproof_core::command_completions(&state.command_buffer);
                state.completion_idx = None;
            }
            if !state.command_completions.is_empty() {
                let idx = match state.completion_idx {
                    Some(0) | None => state.command_completions.len() - 1,
                    Some(i) => i - 1,
                };
                state.completion_idx = Some(idx);
                state.command_buffer = state.command_completions[idx].clone();
                state.command_cursor = state.command_buffer.len();
            }
        }
        KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.command_cursor = 0;
        }
        KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.command_cursor = state.command_buffer.len();
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.command_buffer.drain(..state.command_cursor);
            state.command_cursor = 0;
            state.command_completions =
                openproof_core::command_completions(&state.command_buffer);
            state.completion_idx = None;
        }
        KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if state.command_cursor > 0 {
                let new_pos = openproof_core::delete_word_backward_pos(
                    &state.command_buffer,
                    state.command_cursor,
                );
                state.command_buffer.drain(new_pos..state.command_cursor);
                state.command_cursor = new_pos;
                state.command_completions =
                    openproof_core::command_completions(&state.command_buffer);
                state.completion_idx = None;
            }
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.command_buffer.insert(state.command_cursor, c);
            state.command_cursor += c.len_utf8();
            state.command_completions =
                openproof_core::command_completions(&state.command_buffer);
            state.completion_idx = None;
        }
        KeyCode::Backspace => {
            if state.command_cursor > 0 {
                let prev = state.command_buffer[..state.command_cursor]
                    .char_indices()
                    .next_back()
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                state.command_buffer.remove(prev);
                state.command_cursor = prev;
                state.command_completions =
                    openproof_core::command_completions(&state.command_buffer);
                state.completion_idx = None;
            }
        }
        KeyCode::Left => {
            if state.command_cursor > 0 {
                state.command_cursor = state.command_buffer[..state.command_cursor]
                    .char_indices()
                    .next_back()
                    .map(|(i, _)| i)
                    .unwrap_or(0);
            }
        }
        KeyCode::Right => {
            if state.command_cursor < state.command_buffer.len() {
                state.command_cursor = state.command_buffer[state.command_cursor..]
                    .char_indices()
                    .nth(1)
                    .map(|(i, _)| state.command_cursor + i)
                    .unwrap_or(state.command_buffer.len());
            }
        }
        KeyCode::Delete => {
            if state.command_cursor < state.command_buffer.len() {
                state.command_buffer.remove(state.command_cursor);
                state.command_completions =
                    openproof_core::command_completions(&state.command_buffer);
                state.completion_idx = None;
            }
        }
        KeyCode::Home => {
            state.command_cursor = 0;
        }
        KeyCode::End => {
            state.command_cursor = state.command_buffer.len();
        }
        _ => {}
    }
}
