use crate::state::{AppEvent, AppState, PendingWrite};

/// Replace the `sorry` on the given 1-indexed line with a tactic string.
fn replace_sorry_at_line(content: &str, target_line: usize, tactic: &str) -> String {
    let mut result = String::new();
    let mut replaced = false;
    for (i, line) in content.lines().enumerate() {
        if i + 1 == target_line && !replaced {
            if let Some(pos) = line.find("sorry") {
                result.push_str(&line[..pos]);
                result.push_str(tactic);
                result.push_str(&line[pos + "sorry".len()..]);
                result.push('\n');
                replaced = true;
                continue;
            }
        }
        result.push_str(line);
        result.push('\n');
    }
    result
}

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
                return self.apply_branch_verify_finished(
                    branch_id,
                    focus_node_id,
                    promote,
                    result,
                );
            }

            // --- Content appending ---
            AppEvent::AppendAssistant(content) => {
                return self.apply_append_assistant(content);
            }
            AppEvent::AppendBranchAssistant {
                branch_id,
                content,
                used_tools,
            } => {
                return self.apply_append_branch_assistant(branch_id, content, used_tools);
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

            // --- Tool calls ---
            AppEvent::ToolCallReceived {
                call_id,
                tool_name,
                arguments,
            } => {
                return self.apply_tool_call_received(call_id, tool_name, arguments);
            }
            AppEvent::ToolResultReceived {
                call_id,
                tool_name,
                success,
                output,
            } => {
                return self.apply_tool_result_received(call_id, tool_name, success, output);
            }
            AppEvent::ToolLoopIteration(iteration) => {
                self.tool_loop_iteration = iteration;
            }
            AppEvent::WorkspaceContentSync { content, verified } => {
                return self.apply_workspace_content_sync(content, verified);
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
            // --- Proof goal updates (from Pantograph/tactic search) ---
            AppEvent::ProofGoalUpdated(goal) => {
                if let Some(session) = self.current_session_mut() {
                    // Update existing goal or insert new one
                    if let Some(existing) = session
                        .proof
                        .proof_goals
                        .iter_mut()
                        .find(|g| g.id == goal.id)
                    {
                        *existing = goal;
                    } else {
                        session.proof.proof_goals.push(goal);
                    }
                }
            }
            // --- Tactic search ---
            AppEvent::TacticSearchComplete {
                node_id,
                sorry_line,
                solved,
                tactics,
                remaining_goals,
                expansions,
                search_outcome,
            } => {
                // Store search metrics on the branch for decomposition scoring.
                if let Some(session) = self.current_session_mut() {
                    for branch in &mut session.proof.branches {
                        if branch.focus_node_id.as_deref() == Some(&node_id) {
                            branch
                                .search_history
                                .push(openproof_protocol::SearchAttemptMetrics {
                                    remaining_goals: remaining_goals.unwrap_or(0),
                                    expansions: expansions.unwrap_or(0),
                                    timed_out: search_outcome == "timeout",
                                    outcome: search_outcome.clone(),
                                    timestamp: chrono::Utc::now().to_rfc3339(),
                                });
                            // Keep only last 10 entries to avoid unbounded growth.
                            if branch.search_history.len() > 10 {
                                branch.search_history.remove(0);
                            }
                            break;
                        }
                    }
                }
                // Add a transcript notice so agent branches see tactic search results
                let notice = if solved && !tactics.is_empty() {
                    format!(
                        "Tactic search SOLVED sorry at line {sorry_line}: {}",
                        tactics.join("; ")
                    )
                } else if !tactics.is_empty() {
                    format!(
                        "Tactic search made partial progress at line {sorry_line}: {}",
                        tactics.join("; ")
                    )
                } else {
                    format!(
                        "Tactic search exhausted at line {sorry_line}: standard tactics (simp, omega, ring, linarith, aesop, exact?, apply?) all failed. Try a different approach."
                    )
                };

                if let Some(session) = self.current_session_mut() {
                    session
                        .transcript
                        .push(openproof_protocol::TranscriptEntry {
                            id: format!(
                                "tactic_{sorry_line}_{}",
                                chrono::Utc::now().timestamp_millis()
                            ),
                            role: openproof_protocol::MessageRole::Notice,
                            title: Some("Tactic Search".to_string()),
                            content: notice.clone(),
                            created_at: chrono::Utc::now().to_rfc3339(),
                        });
                }

                if solved && !tactics.is_empty() {
                    let tactic_text = tactics.join("\n  ");
                    self.status = format!("Tactic search solved line {sorry_line}: {tactic_text}");

                    if let Some(session) = self.current_session_mut() {
                        if let Some(node) = session.proof.nodes.iter_mut().find(|n| n.id == node_id)
                        {
                            let patched =
                                replace_sorry_at_line(&node.content, sorry_line, &tactic_text);
                            if patched != node.content {
                                node.content = patched;
                                node.updated_at = chrono::Utc::now().to_rfc3339();
                                node.status = openproof_protocol::ProofNodeStatus::Proving;
                            }
                        }
                        // Mark matching proof goals as closed by BFS (solved_by = None)
                        for goal in &mut session.proof.proof_goals {
                            if goal.sorry_line == Some(sorry_line)
                                && goal.status != openproof_protocol::GoalStatus::Closed
                            {
                                goal.status = openproof_protocol::GoalStatus::Closed;
                                // solved_by stays None = BFS implicit
                            }
                        }
                        session.proof.phase = "verifying".to_string();
                    }
                    if let Some(session) = self.current_session().cloned() {
                        return Some(PendingWrite { session });
                    }
                } else {
                    self.status = notice;
                    if let Some(session) = self.current_session().cloned() {
                        return Some(PendingWrite { session });
                    }
                }
            }
            AppEvent::TacticSearchProgress {
                node_id: _,
                sorry_line,
                expansions,
                best_remaining_goals,
            } => {
                self.status = format!(
                    "Tactic search line {sorry_line}: {expansions} expansions, {best_remaining_goals} goals remaining"
                );
            }

            AppEvent::Quit => {
                self.should_quit = true;
            }
        }
        None
    }
}
