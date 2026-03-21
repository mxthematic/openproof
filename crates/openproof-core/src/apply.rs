use chrono::Utc;
use openproof_protocol::{
    AgentRole, AgentStatus, BranchQueueState, BranchMessage, MessageRole, ProofNodeStatus,
    TranscriptEntry,
};

use crate::commands::delete_word_backward_pos;
use crate::helpers::{agent_role_label, next_id, phase_from_role, summarize_lean_error};
use crate::parser::{derive_goal_label, extract_lean_code_block, parse_assistant_output};
use crate::state::{AppEvent, AppState, FocusPane, PendingWrite};

impl AppState {
    pub fn apply(&mut self, event: AppEvent) -> Option<PendingWrite> {
        match event {
            AppEvent::InputChar(ch) => {
                if self.focus == FocusPane::Composer {
                    self.composer.insert(self.composer_cursor, ch);
                    self.composer_cursor += ch.len_utf8();
                }
            }
            AppEvent::Backspace => {
                if self.focus == FocusPane::Composer && self.composer_cursor > 0 {
                    let prev = self.composer[..self.composer_cursor]
                        .char_indices()
                        .next_back()
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    self.composer.remove(prev);
                    self.composer_cursor = prev;
                }
            }
            AppEvent::CursorLeft => {
                if self.focus == FocusPane::Composer && self.composer_cursor > 0 {
                    self.composer_cursor = self.composer[..self.composer_cursor]
                        .char_indices()
                        .next_back()
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                }
            }
            AppEvent::CursorRight => {
                if self.focus == FocusPane::Composer && self.composer_cursor < self.composer.len() {
                    self.composer_cursor = self.composer[self.composer_cursor..]
                        .char_indices()
                        .nth(1)
                        .map(|(i, _)| self.composer_cursor + i)
                        .unwrap_or(self.composer.len());
                }
            }
            AppEvent::CursorHome => {
                if self.focus == FocusPane::Composer {
                    self.composer_cursor = 0;
                }
            }
            AppEvent::CursorEnd => {
                if self.focus == FocusPane::Composer {
                    self.composer_cursor = self.composer.len();
                }
            }
            AppEvent::DeleteForward => {
                if self.focus == FocusPane::Composer
                    && self.composer_cursor < self.composer.len()
                {
                    self.composer.remove(self.composer_cursor);
                }
            }
            AppEvent::DeleteWordBackward => {
                if self.focus == FocusPane::Composer && self.composer_cursor > 0 {
                    let new_pos =
                        delete_word_backward_pos(&self.composer, self.composer_cursor);
                    self.composer.drain(new_pos..self.composer_cursor);
                    self.composer_cursor = new_pos;
                }
            }
            AppEvent::ClearToStart => {
                if self.focus == FocusPane::Composer {
                    self.composer.drain(..self.composer_cursor);
                    self.composer_cursor = 0;
                }
            }
            AppEvent::Paste(text) => {
                if self.focus == FocusPane::Composer {
                    self.composer.insert_str(self.composer_cursor, &text);
                    self.composer_cursor += text.len();
                }
            }
            AppEvent::TurnStarted => {
                self.turn_in_flight = true;
                self.turn_started_at = Some(std::time::Instant::now());
                self.streaming_text.clear();
                self.status = "Running assistant turn...".to_string();
            }
            AppEvent::ReasoningStarted => {
                self.status = "Reasoning...".to_string();
            }
            AppEvent::StreamDelta(delta) => {
                self.streaming_text.push_str(&delta);
                if self.status.starts_with("Reasoning") {
                    self.status = "Streaming response...".to_string();
                }
            }
            AppEvent::StreamFinished => {
                let text = std::mem::take(&mut self.streaming_text);
                if !text.trim().is_empty() {
                    return self.apply(AppEvent::AppendAssistant(text));
                }
            }
            AppEvent::TurnFinished => {
                // If there's still streaming text that wasn't finalized, flush it
                if !self.streaming_text.is_empty() {
                    let text = std::mem::take(&mut self.streaming_text);
                    if !text.trim().is_empty() {
                        let _ = self.apply(AppEvent::AppendAssistant(text));
                    }
                }
                self.turn_in_flight = false;
                self.turn_started_at = None;
                if !self.verification_in_flight {
                    self.status = "Ready.".to_string();
                }
            }
            AppEvent::LeanVerifyStarted => {
                self.verification_in_flight = true;
                self.status = "Verifying with Lean...".to_string();
                // Add visible notice so user sees verification is happening
                let entry = TranscriptEntry {
                    id: next_id("native_msg"),
                    role: MessageRole::Notice,
                    title: Some("Lean".to_string()),
                    content: "Verifying proof candidate with Lean...".to_string(),
                    created_at: Utc::now().to_rfc3339(),
                };
                if let Some(session) = self.current_session_mut() {
                    session.transcript.push(entry);
                }
            }
            AppEvent::LeanVerifyFinished(result) => {
                self.verification_in_flight = false;
                let now = Utc::now().to_rfc3339();
                if let Some((snapshot, status_line)) = self.current_session_mut().map(|session| {
                    session.updated_at = now.clone();
                    session.proof.last_rendered_scratch = Some(result.rendered_scratch.clone());
                    session.proof.last_verification = Some(result.clone());
                    session.proof.attempt_number = session.proof.attempt_number.saturating_add(1);
                    if !result.scratch_path.is_empty() {
                        session.proof.scratch_path = Some(result.scratch_path.clone());
                    }
                    let active_node_id = session.proof.active_node_id.clone();
                    if let Some(node_id) = active_node_id {
                        if let Some(node) = session.proof.nodes.iter_mut().find(|node| node.id == node_id) {
                            node.status = if result.ok {
                                ProofNodeStatus::Verified
                            } else {
                                ProofNodeStatus::Failed
                            };
                            node.updated_at = now.clone();
                            session.proof.phase = if result.ok {
                                "done".to_string()
                            } else {
                                "repairing".to_string()
                            };
                            session.proof.status_line = if result.ok {
                                format!("Lean verified {}.", node.label)
                            } else {
                                format!("Lean rejected {}.", node.label)
                            };
                        }
                    }
                    if let Some(branch_id) = session.proof.active_branch_id.clone() {
                        if let Some(branch) = session
                            .proof
                            .branches
                            .iter_mut()
                            .find(|branch| branch.id == branch_id)
                        {
                            branch.updated_at = now.clone();
                            branch.attempt_count = branch.attempt_count.saturating_add(1);
                            branch.last_lean_diagnostic = summarize_lean_error(&result);
                            branch.status = if result.ok {
                                AgentStatus::Done
                            } else {
                                AgentStatus::Blocked
                            };
                            branch.phase = Some(if result.ok {
                                "done".to_string()
                            } else {
                                "repairing".to_string()
                            });
                            branch.queue_state = if result.ok {
                                BranchQueueState::Done
                            } else {
                                BranchQueueState::Blocked
                            };
                            branch.search_status = if result.ok {
                                "verified".to_string()
                            } else {
                                "needs repair".to_string()
                            };
                            branch.summary = if result.ok {
                                format!("Lean verified {}.", branch.title)
                            } else {
                                format!("Lean rejected {}.", branch.title)
                            };
                        }
                    }
                    let entry = TranscriptEntry {
                        id: next_id("native_msg"),
                        role: MessageRole::Notice,
                        title: Some(if result.ok {
                            "Lean Verified".to_string()
                        } else {
                            "Lean Failed".to_string()
                        }),
                        content: if result.ok {
                            "Lean verification succeeded.".to_string()
                        } else {
                            summarize_lean_error(&result)
                        },
                        created_at: now,
                    };
                    session.transcript.push(entry);
                    (session.clone(), session.proof.status_line.clone())
                }) {
                    self.pending_writes += 1;
                    self.status = status_line;
                    return Some(PendingWrite { session: snapshot });
                }
            }
            AppEvent::BranchVerifyFinished {
                branch_id,
                focus_node_id,
                promote,
                result,
            } => {
                self.verification_in_flight = false;
                let now = Utc::now().to_rfc3339();
                if let Some(snapshot) = self.current_session_mut().map(|session| {
                    let root_node_id = session.proof.root_node_id.clone();
                    let focus_node_id = focus_node_id
                        .or_else(|| {
                            session
                                .proof
                                .branches
                                .iter()
                                .find(|branch| branch.id == branch_id)
                                .and_then(|branch| branch.focus_node_id.clone())
                        })
                        .or_else(|| session.proof.active_node_id.clone());
                    if let Some(node_id) = focus_node_id.as_deref() {
                        if let Some(node) = session.proof.nodes.iter_mut().find(|node| node.id == node_id) {
                            node.status = if result.ok {
                                ProofNodeStatus::Verified
                            } else {
                                ProofNodeStatus::Failed
                            };
                            node.updated_at = now.clone();
                        }
                    }

                    let diagnostic = summarize_lean_error(&result);
                    let mut status_line = if result.ok {
                        "Lean accepted the latest branch candidate.".to_string()
                    } else {
                        "Lean rejected the latest branch candidate.".to_string()
                    };
                    let mut should_resolve = false;
                    let mut should_refresh_hidden = false;
                    let mut promote_target: Option<String> = None;
                    if let Some(branch) = session
                        .proof
                        .branches
                        .iter_mut()
                        .find(|branch| branch.id == branch_id)
                    {
                        branch.updated_at = now.clone();
                        branch.attempt_count = branch.attempt_count.saturating_add(1);
                        branch.latest_diagnostics = if result.ok {
                            None
                        } else {
                            Some(diagnostic.clone())
                        };
                        branch.last_lean_diagnostic = if result.ok {
                            String::new()
                        } else {
                            diagnostic.clone()
                        };
                        branch.last_successful_check_at = if result.ok {
                            Some(result.checked_at.clone())
                        } else {
                            branch.last_successful_check_at.clone()
                        };
                        branch.phase = Some(if result.ok {
                            "proving".to_string()
                        } else {
                            "repairing".to_string()
                        });
                        branch.progress_kind = Some(if result.ok {
                            if focus_node_id
                                .as_deref()
                                .zip(root_node_id.as_deref())
                                .map(|(focus, root)| focus == root)
                                .unwrap_or(false)
                            {
                                "verified".to_string()
                            } else {
                                "candidate".to_string()
                            }
                        } else {
                            "repairing".to_string()
                        });
                        branch.status = if result.ok {
                            AgentStatus::Done
                        } else {
                            AgentStatus::Blocked
                        };
                        branch.queue_state = if result.ok {
                            BranchQueueState::Done
                        } else {
                            BranchQueueState::Blocked
                        };
                        branch.search_status = if result.ok {
                            "verified".to_string()
                        } else {
                            "needs repair".to_string()
                        };
                        let root_target_verified = focus_node_id
                            .as_deref()
                            .zip(root_node_id.as_deref())
                            .map(|(focus, root)| focus == root)
                            .unwrap_or(false);
                        branch.score = if result.ok {
                            if root_target_verified {
                                100.0
                            } else {
                                branch.score.max(72.0 + (branch.attempt_count.min(8) as f32 * 3.0))
                            }
                        } else {
                            (branch.score - 4.0).max(0.0)
                        };
                        branch.summary = if result.ok {
                            format!("Lean verified {}.", branch.title)
                        } else {
                            format!("Lean rejected {}.", branch.title)
                        };
                        should_resolve = result.ok && root_target_verified;
                        should_refresh_hidden = branch.hidden;
                        if promote || should_resolve {
                            promote_target = Some(branch.id.clone());
                        }
                        status_line = branch.summary.clone();
                    }

                    session.updated_at = now.clone();
                    session.proof.latest_diagnostics = if result.ok {
                        None
                    } else {
                        Some(diagnostic.clone())
                    };
                    session.proof.goal_summary = focus_node_id
                        .as_deref()
                        .and_then(|node_id| session.proof.nodes.iter().find(|node| node.id == node_id))
                        .map(|node| node.statement.clone())
                        .or_else(|| session.proof.goal_summary.clone());
                    session.proof.status_line = status_line.clone();
                    session.proof.phase = if should_resolve {
                        "done".to_string()
                    } else if result.ok {
                        "proving".to_string()
                    } else {
                        "repairing".to_string()
                    };

                    session.transcript.push(TranscriptEntry {
                        id: next_id("native_msg"),
                        role: MessageRole::Notice,
                        title: Some(if result.ok {
                            "Branch Verified".to_string()
                        } else {
                            "Branch Needs Repair".to_string()
                        }),
                        content: if result.ok { status_line.clone() } else { diagnostic.clone() },
                        created_at: now.clone(),
                    });

                    if let Some(promote_branch_id) = promote_target {
                        let previous_active = session.proof.active_foreground_branch_id.clone();
                        if let Some(previous_id) =
                            previous_active.as_deref().filter(|id| *id != promote_branch_id)
                        {
                            if let Some(previous) = session
                                .proof
                                .branches
                                .iter_mut()
                                .find(|branch| branch.id == previous_id)
                            {
                                previous.hidden = true;
                                previous.branch_kind = agent_role_label(previous.role).to_string();
                                previous.superseded_by_branch_id = Some(promote_branch_id.clone());
                                previous.updated_at = now.clone();
                            }
                        }
                        let mut promoted_role = None;
                        let mut promoted_focus_node_id = None;
                        if let Some(promoted) = session
                            .proof
                            .branches
                            .iter_mut()
                            .find(|branch| branch.id == promote_branch_id)
                        {
                            let promoted_from_hidden = promoted.hidden || promoted.promoted_from_hidden;
                            promoted.hidden = false;
                            promoted.branch_kind = "foreground".to_string();
                            promoted.promoted_from_hidden = promoted_from_hidden;
                            promoted.superseded_by_branch_id = None;
                            promoted_role = Some(promoted.role);
                            promoted_focus_node_id = promoted.focus_node_id.clone();
                        }
                        session.proof.active_foreground_branch_id = Some(promote_branch_id.clone());
                        session.proof.active_branch_id = Some(promote_branch_id.clone());
                        session.proof.active_agent_role = promoted_role;
                        if let Some(node_id) = promoted_focus_node_id {
                            session.proof.active_node_id = Some(node_id);
                        }
                    }

                    if should_resolve {
                        session.proof.resolved_by_branch_id = Some(branch_id.clone());
                        session.proof.is_autonomous_running = false;
                        session.proof.autonomous_pause_reason = None;
                        session.proof.autonomous_stop_reason =
                            Some("Autonomous loop completed the current proof run.".to_string());
                    } else if should_refresh_hidden {
                        let mut hidden = session
                            .proof
                            .branches
                            .iter()
                            .filter(|branch| branch.hidden)
                            .cloned()
                            .collect::<Vec<_>>();
                        hidden.sort_by(|left, right| {
                            right
                                .score
                                .partial_cmp(&left.score)
                                .unwrap_or(std::cmp::Ordering::Equal)
                                .then_with(|| right.updated_at.cmp(&left.updated_at))
                        });
                        session.proof.hidden_branch_count = hidden.len();
                        session.proof.hidden_best_branch_id =
                            hidden.first().map(|branch| branch.id.clone());
                    }
                    session.clone()
                }) {
                    self.pending_writes += 1;
                    self.status = snapshot.proof.status_line.clone();
                    return Some(PendingWrite { session: snapshot });
                }
            }
            AppEvent::AppendAssistant(content) => {
                let text = content.trim();
                if !text.is_empty() {
                    let parsed = parse_assistant_output(text);
                    let entry = TranscriptEntry {
                        id: next_id("native_msg"),
                        role: MessageRole::Assistant,
                        title: Some("OpenProof".to_string()),
                        content: text.to_string(),
                        created_at: Utc::now().to_rfc3339(),
                    };
                    if let Some((snapshot, status_line)) = self.current_session_mut().map(|session| {
                        session.updated_at = entry.created_at.clone();
                        session.transcript.push(entry);
                        if let Some(title) = parsed.title.as_ref().filter(|item| !item.trim().is_empty()) {
                            session.title = title.trim().to_string();
                        }
                        if let Some(problem) = parsed.problem.as_ref().filter(|item| !item.trim().is_empty()) {
                            session.proof.problem = Some(problem.trim().to_string());
                        }
                        if let Some(formal_target) = parsed.formal_target.as_ref().filter(|item| !item.trim().is_empty()) {
                            session.proof.formal_target = Some(formal_target.trim().to_string());
                        }
                        if let Some(accepted_target) = parsed
                            .accepted_target
                            .as_ref()
                            .filter(|item| !item.trim().is_empty())
                        {
                            session.proof.accepted_target = Some(accepted_target.trim().to_string());
                            session.proof.pending_question = None;
                            session.proof.awaiting_clarification = false;
                        }
                        if let Some(search_status) = parsed.search_status.as_ref().filter(|item| !item.trim().is_empty()) {
                            session.proof.search_status = Some(search_status.trim().to_string());
                        }
                        if let Some(phase) = parsed.phase.as_ref().filter(|item| !item.trim().is_empty()) {
                            session.proof.phase = phase.trim().to_string();
                        }
                        if !parsed.assumptions.is_empty() {
                            session.proof.assumptions = parsed.assumptions.clone();
                        }
                        if !parsed.paper_notes.is_empty() {
                            session.proof.paper_notes.extend(parsed.paper_notes.clone());
                        }
                        if let Some(ref tex) = parsed.paper_tex {
                            session.proof.paper_tex = tex.clone();
                        }
                        if let Some(question) = parsed.question.clone() {
                            session.proof.status_line = format!("Awaiting clarification: {}", question.prompt);
                            session.proof.phase = "formalizing".to_string();
                            session.proof.pending_question = Some(question);
                            session.proof.awaiting_clarification = true;
                        }
                        for created in &parsed.created_nodes {
                            let node = openproof_protocol::ProofNode {
                                id: next_id("node"),
                                kind: created.kind,
                                label: created.label.clone(),
                                statement: created.statement.clone(),
                                content: String::new(),
                                status: ProofNodeStatus::Pending,
                                created_at: session.updated_at.clone(),
                                updated_at: session.updated_at.clone(),
                            };
                            session.proof.active_node_id = Some(node.id.clone());
                            if session.proof.root_node_id.is_none() {
                                session.proof.root_node_id = Some(node.id.clone());
                            }
                            session.proof.nodes.push(node);
                        }
                        if session.proof.nodes.is_empty() {
                            if let Some(target) = session
                                .proof
                                .accepted_target
                                .clone()
                                .or_else(|| session.proof.formal_target.clone())
                            {
                                let label = derive_goal_label(
                                    parsed.title.as_deref().or(Some(session.title.as_str())).unwrap_or("Goal"),
                                );
                                let node = openproof_protocol::ProofNode {
                                    id: next_id("node"),
                                    kind: openproof_protocol::ProofNodeKind::Theorem,
                                    label,
                                    statement: target,
                                    content: String::new(),
                                    status: ProofNodeStatus::Pending,
                                    created_at: session.updated_at.clone(),
                                    updated_at: session.updated_at.clone(),
                                };
                                session.proof.active_node_id = Some(node.id.clone());
                                if session.proof.root_node_id.is_none() {
                                    session.proof.root_node_id = Some(node.id.clone());
                                }
                                session.proof.nodes.push(node);
                            }
                        }
                        if let Some(candidate) = parsed
                            .lean_snippets
                            .first()
                            .cloned()
                            .or_else(|| extract_lean_code_block(text))
                        {
                            let active_node_id = session.proof.active_node_id.clone();
                            if let Some(node_id) = active_node_id {
                                if let Some(node) =
                                    session.proof.nodes.iter_mut().find(|node| node.id == node_id)
                                {
                                    node.content = candidate;
                                    node.status = ProofNodeStatus::Proving;
                                    node.updated_at = session.updated_at.clone();
                                    session.proof.phase = "proving".to_string();
                                    session.proof.goal_summary = Some(node.statement.clone());
                                    session.proof.status_line =
                                        format!("Updated Lean candidate for {}.", node.label);
                                }
                            }
                        } else if let Some(next_step) = parsed.next_steps.first().filter(|item| !item.trim().is_empty()) {
                            session.proof.status_line = next_step.trim().to_string();
                        }
                        (session.clone(), session.proof.status_line.clone())
                    }) {
                        self.sync_question_selection();
                        self.pending_writes += 1;
                        self.status = status_line;
                        return Some(PendingWrite { session: snapshot });
                    }
                }
            }
            AppEvent::AppendBranchAssistant { branch_id, content } => {
                let text = content.trim();
                if !text.is_empty() {
                    let parsed = parse_assistant_output(text);
                    if let Some((snapshot, status_line)) = self.current_session_mut().map(|session| {
                        let now = Utc::now().to_rfc3339();
                        session.updated_at = now.clone();
                        let active_node_id = session.proof.active_node_id.clone();
                        if let Some(branch) = session
                            .proof
                            .branches
                            .iter_mut()
                            .find(|branch| branch.id == branch_id)
                        {
                            branch.updated_at = now.clone();
                            branch.transcript.push(BranchMessage {
                                id: next_id("branchmsg"),
                                role: MessageRole::Assistant,
                                content: text.to_string(),
                                created_at: now.clone(),
                            });
                            if let Some(search_status) =
                                parsed.search_status.as_ref().filter(|item| !item.trim().is_empty())
                            {
                                branch.search_status = search_status.trim().to_string();
                            }
                            if let Some(phase) = parsed.phase.as_ref().filter(|item| !item.trim().is_empty()) {
                                branch.phase = Some(phase.trim().to_string());
                                branch.progress_kind = Some(phase.trim().to_string());
                            }
                            if let Some(snippet) = parsed
                                .lean_snippets
                                .first()
                                .cloned()
                                .or_else(|| extract_lean_code_block(text))
                            {
                                branch.lean_snippet = snippet.clone();
                                branch.search_status = "candidate updated".to_string();
                                branch.progress_kind = Some(if branch.role == AgentRole::Repairer {
                                    "repairing".to_string()
                                } else {
                                    "candidate".to_string()
                                });
                                if let Some(focus_node_id) = branch.focus_node_id.clone().or(active_node_id) {
                                    if let Some(node) = session
                                        .proof
                                        .nodes
                                        .iter_mut()
                                        .find(|node| node.id == focus_node_id)
                                    {
                                        node.content = snippet;
                                        node.status = ProofNodeStatus::Proving;
                                        node.updated_at = now.clone();
                                        session.proof.active_node_id = Some(node.id.clone());
                                    }
                                }
                            }
                            if !parsed.next_steps.is_empty() {
                                branch.summary = parsed.next_steps.join(" ");
                            } else if !parsed.paper_notes.is_empty() {
                                branch.summary = parsed.paper_notes.join(" ");
                            }
                            if branch.role == AgentRole::Planner && !branch.summary.trim().is_empty() {
                                session.proof.strategy_summary = Some(branch.summary.clone());
                            }
                            if !branch.hidden {
                                session.proof.active_branch_id = Some(branch.id.clone());
                                session.proof.active_agent_role = Some(branch.role);
                                session.proof.phase = branch
                                    .phase
                                    .clone()
                                    .unwrap_or_else(|| phase_from_role(branch.role).to_string());
                                session.proof.goal_summary = branch
                                    .latest_goals
                                    .clone()
                                    .or_else(|| (!branch.goal_summary.trim().is_empty()).then(|| branch.goal_summary.clone()));
                                session.proof.status_line = format!(
                                    "{} branch updated {}.",
                                    agent_role_label(branch.role),
                                    branch.title
                                );
                            }
                            return (session.clone(), session.proof.status_line.clone());
                        }
                        (session.clone(), "Branch update received.".to_string())
                    }) {
                        self.pending_writes += 1;
                        self.status = status_line;
                        return Some(PendingWrite { session: snapshot });
                    }
                }
            }
            AppEvent::FinishBranch {
                branch_id,
                status,
                summary,
                output,
            } => {
                if let Some((snapshot, status_line)) = self.current_session_mut().map(|session| {
                    let now = Utc::now().to_rfc3339();
                    session.updated_at = now.clone();
                    if let Some(branch) = session
                        .proof
                        .branches
                        .iter_mut()
                        .find(|branch| branch.id == branch_id)
                    {
                        branch.updated_at = now.clone();
                        branch.status = status;
                        branch.queue_state = match status {
                            AgentStatus::Done => BranchQueueState::Done,
                            AgentStatus::Error => BranchQueueState::Error,
                            AgentStatus::Blocked => BranchQueueState::Blocked,
                            AgentStatus::Running => BranchQueueState::Running,
                            AgentStatus::Idle => BranchQueueState::Queued,
                        };
                        branch.summary = summary.clone();
                        branch.diagnostics = output.clone();
                        branch.transcript.push(BranchMessage {
                            id: next_id("branchmsg"),
                            role: MessageRole::System,
                            content: format!("Branch {}: {}", format_agent_status(status), summary),
                            created_at: now.clone(),
                        });
                    }
                    for agent in &mut session.proof.agents {
                        if agent.branch_ids.iter().any(|id| id == &branch_id) {
                            agent.status = status;
                            agent.updated_at = now.clone();
                            if let Some(task) = agent
                                .tasks
                                .iter_mut()
                                .find(|task| task.branch_id.as_deref() == Some(branch_id.as_str()))
                            {
                                task.status = status;
                                task.output = output.clone();
                                task.updated_at = now.clone();
                            }
                        }
                    }
                    session.proof.status_line = summary.clone();
                    (session.clone(), summary)
                }) {
                    self.pending_writes += 1;
                    self.status = status_line;
                    return Some(PendingWrite { session: snapshot });
                }
            }
            AppEvent::AppendNotice { title, content } => {
                let entry = TranscriptEntry {
                    id: next_id("native_msg"),
                    role: MessageRole::Notice,
                    title: Some(title),
                    content,
                    created_at: Utc::now().to_rfc3339(),
                };
                if let Some(snapshot) = self.current_session_mut().map(|session| {
                    session.updated_at = entry.created_at.clone();
                    session.transcript.push(entry);
                    session.clone()
                }) {
                    self.sync_question_selection();
                    self.pending_writes += 1;
                    self.status = "Local command applied.".to_string();
                    return Some(PendingWrite { session: snapshot });
                }
            }
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
            AppEvent::AuthLoaded(auth) => {
                self.auth = auth;
                self.status = "Loaded OpenProof auth summary in the background.".to_string();
            }
            AppEvent::LeanLoaded(lean) => {
                self.lean = lean;
                self.status = "Loaded Lean toolchain health in the background.".to_string();
            }
            AppEvent::SyncCompleted => {
                if let Ok(write) = self.mark_sync_completed() {
                    return Some(write);
                }
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

fn format_agent_status(status: AgentStatus) -> &'static str {
    crate::helpers::format_agent_status(status)
}
