use crate::state::{AppEvent, AppState, PendingWrite};

impl AppState {
    pub fn apply(&mut self, event: AppEvent) -> Option<PendingWrite> {
        match event {
            // --- Composer / text input ---
            AppEvent::InputChar(ch) => {
                self.apply_input_char(ch);
            }
            AppEvent::Backspace => {
                self.apply_backspace();
            }
            AppEvent::CursorLeft => {
                self.apply_cursor_left();
            }
            AppEvent::CursorRight => {
                self.apply_cursor_right();
            }
            AppEvent::CursorHome => {
                self.apply_cursor_home();
            }
            AppEvent::CursorEnd => {
                self.apply_cursor_end();
            }
            AppEvent::DeleteForward => {
                self.apply_delete_forward();
            }
            AppEvent::DeleteWordBackward => {
                self.apply_delete_word_backward();
            }
            AppEvent::ClearToStart => {
                self.apply_clear_to_start();
            }
            AppEvent::Paste(text) => {
                self.apply_paste(text);
            }

            // --- Turn / streaming ---
            AppEvent::TurnStarted => {
                self.apply_turn_started();
            }
            AppEvent::ReasoningStarted => {
                self.apply_reasoning_started();
            }
            AppEvent::StreamDelta(delta) => {
                self.apply_stream_delta(delta);
            }
            AppEvent::StreamFinished => {
                return self.apply_stream_finished();
            }
            AppEvent::TurnFinished => {
                self.apply_turn_finished();
            }

            // --- Lean verification ---
            AppEvent::LeanVerifyStarted => {
                self.apply_lean_verify_started();
            }
            AppEvent::LeanVerifyFinished(result) => {
                return self.apply_lean_verify_finished(result);
            }
            AppEvent::BranchVerifyFinished {
                branch_id,
                focus_node_id,
                promote,
                result,
            } => {
                return self.apply_branch_verify_finished(branch_id, focus_node_id, promote, result);
            }

            // --- Content appending ---
            AppEvent::AppendAssistant(content) => {
                return self.apply_append_assistant(content);
            }
            AppEvent::AppendBranchAssistant { branch_id, content } => {
                return self.apply_append_branch_assistant(branch_id, content);
            }
            AppEvent::FinishBranch {
                branch_id,
                status,
                summary,
                output,
            } => {
                return self.apply_finish_branch(branch_id, status, summary, output);
            }
            AppEvent::AppendNotice { title, content } => {
                return self.apply_append_notice(title, content);
            }

            // --- UI / navigation ---
            AppEvent::FocusNext => {
                self.focus = self.focus.next();
            }
            AppEvent::ToggleProofPane => {
                self.show_proof_pane = !self.show_proof_pane;
                self.status = if self.show_proof_pane {
                    "Opened proof pane.".to_string()
                } else {
                    "Closed proof pane.".to_string()
                };
            }
            AppEvent::SelectPrevQuestionOption => {
                if let Some(question) = self.pending_question() {
                    if !question.options.is_empty() {
                        self.selected_question_option =
                            self.selected_question_option.saturating_sub(1);
                        if let Some(option) = self.selected_question_option() {
                            self.status = format!("Clarification option: {}.", option.label);
                        }
                    }
                }
            }
            AppEvent::SelectNextQuestionOption => {
                if let Some(question) = self.pending_question() {
                    if !question.options.is_empty() {
                        self.selected_question_option = self
                            .selected_question_option
                            .saturating_add(1)
                            .min(question.options.len().saturating_sub(1));
                        if let Some(option) = self.selected_question_option() {
                            self.status = format!("Clarification option: {}.", option.label);
                        }
                    }
                }
            }
            AppEvent::SelectPrevSession => {
                if self.selected_session > 0 {
                    self.selected_session -= 1;
                    self.scroll_offset = 0;
                    self.sync_question_selection();
                }
            }
            AppEvent::SelectNextSession => {
                if self.selected_session + 1 < self.sessions.len() {
                    self.selected_session += 1;
                    self.scroll_offset = 0;
                    self.sync_question_selection();
                }
            }
            AppEvent::ScrollTranscriptUp => {
                let max = self.total_visual_lines.saturating_sub(self.visible_height);
                self.scroll_offset = (self.scroll_offset + 1).min(max);
            }
            AppEvent::ScrollTranscriptDown => {
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
            }
            AppEvent::ScrollPageUp => {
                let max = self.total_visual_lines.saturating_sub(self.visible_height);
                self.scroll_offset = (self.scroll_offset + 20).min(max);
            }
            AppEvent::ScrollPageDown => {
                self.scroll_offset = self.scroll_offset.saturating_sub(20);
            }
            AppEvent::ScrollToTop => {
                let max = self.total_visual_lines.saturating_sub(self.visible_height);
                self.scroll_offset = max;
            }
            AppEvent::ScrollToBottom => {
                self.scroll_offset = 0;
            }

            // --- Background loads and lifecycle ---
            AppEvent::AuthLoaded(auth) => {
                self.auth = auth;
                self.status = "Loaded OpenProof auth summary in the background.".to_string();
            }
            AppEvent::LeanLoaded(lean) => {
                self.lean = lean;
                self.status = "Loaded Lean toolchain health in the background.".to_string();
            }
            AppEvent::SyncCompleted => {
                return self.apply_sync_completed();
            }
            AppEvent::AutonomousTick => {}
            AppEvent::PersistSucceeded(session_id) => {
                self.pending_writes = self.pending_writes.saturating_sub(1);
                self.status = format!("Persisted local session update for {session_id}.");
            }
            AppEvent::PersistFailed(error) => {
                self.pending_writes = self.pending_writes.saturating_sub(1);
                self.status = format!("Background persistence failed: {error}");
            }
            AppEvent::Quit => {
                self.should_quit = true;
            }
        }
        None
    }
}
