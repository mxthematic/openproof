use chrono::Utc;
use openproof_protocol::{
    AgentRecord, AgentRole, AgentStatus, AgentTask, BranchMessage, BranchQueueState, CloudPolicy,
    LeanHealth, LeanVerificationSummary, MessageRole, ProofBranch, ProofNode, ProofNodeKind,
    ProofNodeStatus, ProofQuestionOption, ProofQuestionState, ProofSessionState, SessionSnapshot,
    ShareMode, TranscriptEntry, AuthSummary,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusPane {
    Sessions,
    Transcript,
    Composer,
}

impl FocusPane {
    pub fn next(self) -> Self {
        match self {
            Self::Sessions => Self::Transcript,
            Self::Transcript => Self::Composer,
            Self::Composer => Self::Sessions,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PendingWrite {
    pub session: SessionSnapshot,
}

#[derive(Debug, Clone)]
pub struct SubmittedInput {
    pub session_id: String,
    pub raw_text: String,
    pub user_entry: TranscriptEntry,
    pub session_snapshot: SessionSnapshot,
}

#[derive(Debug, Clone, Default)]
pub struct AutonomousRunPatch {
    pub is_autonomous_running: Option<bool>,
    pub autonomous_iteration_count: Option<usize>,
    pub autonomous_started_at: Option<Option<String>>,
    pub autonomous_last_progress_at: Option<Option<String>>,
    pub autonomous_pause_reason: Option<Option<String>>,
    pub autonomous_stop_reason: Option<Option<String>>,
}

#[derive(Debug, Clone)]
pub enum AppEvent {
    InputChar(char),
    Backspace,
    CursorLeft,
    CursorRight,
    CursorHome,
    CursorEnd,
    DeleteForward,
    DeleteWordBackward,
    ClearToStart,
    Paste(String),
    TurnStarted,
    TurnFinished,
    LeanVerifyStarted,
    LeanVerifyFinished(LeanVerificationSummary),
    BranchVerifyFinished {
        branch_id: String,
        focus_node_id: Option<String>,
        promote: bool,
        result: LeanVerificationSummary,
    },
    StreamDelta(String),
    StreamFinished,
    ReasoningStarted,
    AppendAssistant(String),
    AppendBranchAssistant { branch_id: String, content: String },
    FinishBranch {
        branch_id: String,
        status: AgentStatus,
        summary: String,
        output: String,
    },
    AutonomousTick,
    AppendNotice { title: String, content: String },
    FocusNext,
    ToggleProofPane,
    SelectPrevQuestionOption,
    SelectNextQuestionOption,
    SelectPrevSession,
    SelectNextSession,
    ScrollTranscriptUp,
    ScrollTranscriptDown,
    ScrollPageUp,
    ScrollPageDown,
    ScrollToTop,
    ScrollToBottom,
    AuthLoaded(AuthSummary),
    LeanLoaded(LeanHealth),
    SyncCompleted,
    PersistSucceeded(String),
    PersistFailed(String),
    Quit,
}

/// Modal overlays rendered on top of the main UI.
#[derive(Debug, Clone)]
pub enum Overlay {
    /// Interactive session picker for /resume and /sessions.
    SessionPicker {
        /// Index into `AppState::sessions`.
        selected: usize,
    },
    /// Interactive node/branch picker for /focus.
    FocusPicker {
        /// (id, label, kind) tuples for all focusable targets.
        items: Vec<(String, String, String)>,
        selected: usize,
    },
}

#[derive(Debug, Clone)]
pub struct AppState {
    pub sessions: Vec<SessionSnapshot>,
    pub selected_session: usize,
    pub workspace_root: Option<String>,
    pub workspace_label: Option<String>,
    pub focus: FocusPane,
    pub composer: String,
    pub composer_cursor: usize,
    pub scroll_offset: usize,
    pub total_visual_lines: usize,
    pub visible_height: usize,
    pub show_proof_pane: bool,
    pub selected_question_option: usize,
    pub status: String,
    pub should_quit: bool,
    pub auth: AuthSummary,
    pub lean: LeanHealth,
    pub pending_writes: usize,
    pub turn_in_flight: bool,
    pub verification_in_flight: bool,
    pub turn_started_at: Option<std::time::Instant>,
    /// Whether the command bar (/ mode) is active.
    pub command_mode: bool,
    pub command_buffer: String,
    pub command_cursor: usize,
    pub command_completions: Vec<String>,
    pub completion_idx: Option<usize>,
    pub overlay: Option<Overlay>,
    /// How many transcript entries have been flushed to terminal scrollback.
    pub flushed_turn_count: usize,
    /// Accumulates streaming model output before it's finalized.
    pub streaming_text: String,
}

impl AppState {
    pub fn new(
        mut sessions: Vec<SessionSnapshot>,
        status: String,
        workspace_root: Option<String>,
        workspace_label: Option<String>,
    ) -> Self {
        if sessions.is_empty() {
            sessions.push(default_session_with_workspace(
                workspace_root.as_deref(),
                workspace_label.as_deref(),
            ));
        }
        Self {
            sessions,
            selected_session: 0,
            workspace_root,
            workspace_label,
            focus: FocusPane::Composer,
            composer: String::new(),
            composer_cursor: 0,
            scroll_offset: 0,
            total_visual_lines: 0,
            visible_height: 0,
            show_proof_pane: false,
            selected_question_option: 0,
            status,
            should_quit: false,
            auth: AuthSummary::default(),
            lean: LeanHealth::default(),
            pending_writes: 0,
            turn_in_flight: false,
            verification_in_flight: false,
            turn_started_at: None,
            command_mode: false,
            command_buffer: String::new(),
            command_cursor: 0,
            command_completions: Vec::new(),
            completion_idx: None,
            overlay: None,
            flushed_turn_count: 0,
            streaming_text: String::new(),
        }
    }

    pub fn current_session(&self) -> Option<&SessionSnapshot> {
        self.sessions.get(self.selected_session)
    }

    pub fn current_session_mut(&mut self) -> Option<&mut SessionSnapshot> {
        self.sessions.get_mut(self.selected_session)
    }

    pub fn active_proof_node(&self) -> Option<&ProofNode> {
        let session = self.current_session()?;
        let active_id = session.proof.active_node_id.as_deref()?;
        session.proof.nodes.iter().find(|node| node.id == active_id)
    }

    pub fn pending_question(&self) -> Option<&ProofQuestionState> {
        self.current_session()?.proof.pending_question.as_ref()
    }

    pub fn has_open_question(&self) -> bool {
        self.pending_question()
            .map(|question| question.status != "resolved" && !question.options.is_empty())
            .unwrap_or(false)
    }

    pub fn selected_question_option(&self) -> Option<&ProofQuestionOption> {
        let question = self.pending_question()?;
        if question.options.is_empty() {
            return None;
        }
        let index = self
            .selected_question_option
            .min(question.options.len().saturating_sub(1));
        question.options.get(index)
    }

    pub fn sync_question_selection(&mut self) {
        let Some(question) = self.pending_question() else {
            self.selected_question_option = 0;
            return;
        };
        if question.options.is_empty() {
            self.selected_question_option = 0;
            return;
        }
        if let Some(recommended) = question.recommended_option_id.as_ref() {
            if let Some(index) = question.options.iter().position(|option| &option.id == recommended) {
                self.selected_question_option = index;
                return;
            }
        }
        self.selected_question_option = self
            .selected_question_option
            .min(question.options.len().saturating_sub(1));
    }

    pub fn submit_composer(&mut self) -> Option<SubmittedInput> {
        let text = self.composer.trim().to_string();
        self.composer.clear();
        self.composer_cursor = 0;
        self.submit_text(text)
    }

    pub fn submit_text(&mut self, text: String) -> Option<SubmittedInput> {
        let text = text.trim().to_string();
        if text.is_empty() {
            return None;
        }
        let now = Utc::now().to_rfc3339();
        let entry = TranscriptEntry {
            id: next_id("native_msg"),
            role: MessageRole::User,
            title: None,
            content: text.clone(),
            created_at: now.clone(),
        };
        if let Some(session) = self.current_session_mut() {
            session.updated_at = now;
            if let Some(question) = session.proof.pending_question.as_mut() {
                if question.status != "resolved" {
                    question.answer_text = Some(text.clone());
                    question.status = "answered".to_string();
                }
            }
            session.transcript.push(entry.clone());
            let session_snapshot = session.clone();
            let submitted = SubmittedInput {
                session_id: session.id.clone(),
                raw_text: text,
                user_entry: entry,
                session_snapshot,
            };
            self.pending_writes += 1;
            return Some(submitted);
        }
        None
    }

    pub fn add_proof_node(
        &mut self,
        kind: ProofNodeKind,
        label: &str,
        statement: &str,
    ) -> Result<PendingWrite, String> {
        let label = label.trim();
        let statement = statement.trim();
        if label.is_empty() || statement.is_empty() {
            return Err("Usage: /theorem <label> :: <statement> or /lemma <label> :: <statement>".to_string());
        }
        let timestamp = Utc::now().to_rfc3339();
        let snapshot = {
            let Some(session) = self.current_session_mut() else {
                return Err("No active session.".to_string());
            };
            let node = ProofNode {
                id: next_id("node"),
                kind,
                label: label.to_string(),
                statement: statement.to_string(),
                content: String::new(),
                status: ProofNodeStatus::Pending,
                created_at: timestamp.clone(),
                updated_at: timestamp.clone(),
            };
            session.proof.active_node_id = Some(node.id.clone());
            if session.proof.root_node_id.is_none() {
                session.proof.root_node_id = Some(node.id.clone());
            }
            session.proof.phase = "formalizing".to_string();
            session.proof.status_line = format!("Focused {}.", node.label);
            session.updated_at = timestamp.clone();
            session.proof.nodes.push(node.clone());
            let title = match kind {
                ProofNodeKind::Theorem => "Theorem Added",
                ProofNodeKind::Lemma => "Lemma Added",
                ProofNodeKind::Artifact => "Artifact Added",
                ProofNodeKind::Attempt => "Attempt Added",
                ProofNodeKind::Conjecture => "Conjecture Added",
            };
            let entry = TranscriptEntry {
                id: next_id("native_msg"),
                role: MessageRole::Notice,
                title: Some(title.to_string()),
                content: format!("{} :: {}", node.label, node.statement),
                created_at: timestamp,
            };
            session.transcript.push(entry.clone());
            session.clone()
        };
        self.pending_writes += 1;
        self.status = "Added proof node.".to_string();
        Ok(PendingWrite { session: snapshot })
    }

    pub fn set_active_node(&mut self, node_id: Option<&str>) -> Result<Option<PendingWrite>, String> {
        let timestamp = Utc::now().to_rfc3339();
        let snapshot = {
            let Some(session) = self.current_session_mut() else {
                return Err("No active session.".to_string());
            };
            match node_id {
                Some("clear") | None => {
                    session.proof.active_node_id = None;
                    session.proof.status_line = "Cleared active proof focus.".to_string();
                }
                Some(node_id) => {
                    let node = session
                        .proof
                        .nodes
                        .iter()
                        .find(|node| node.id == node_id)
                        .cloned()
                        .ok_or_else(|| format!("Proof node not found: {node_id}"))?;
                    session.proof.active_node_id = Some(node.id);
                    session.proof.status_line = format!("Focused {}.", node.label);
                }
            }
            session.updated_at = timestamp;
            session.clone()
        };
        self.pending_writes += 1;
        Ok(Some(PendingWrite { session: snapshot }))
    }

    pub fn status_report(&self) -> String {
        let session = match self.current_session() {
            Some(session) => session,
            None => return "No active session.".to_string(),
        };
        let active_node = self.active_proof_node();
        let active_branch = session
            .proof
            .active_branch_id
            .as_deref()
            .and_then(|id| session.proof.branches.iter().find(|branch| branch.id == id));
        let verified = session
            .proof
            .nodes
            .iter()
            .filter(|node| node.status == ProofNodeStatus::Verified)
            .count();
        let failing = session
            .proof
            .nodes
            .iter()
            .filter(|node| node.status == ProofNodeStatus::Failed)
            .count();
        [
            format!("Session: {}", session.title),
            format!("Share mode: {:?}", session.cloud.share_mode),
            format!("Sync enabled: {}", if session.cloud.sync_enabled { "yes" } else { "no" }),
            format!("Proof phase: {}", session.proof.phase),
            format!("Status: {}", session.proof.status_line),
            format!(
                "Problem: {}",
                session
                    .proof
                    .problem
                    .clone()
                    .unwrap_or_else(|| "none".to_string())
            ),
            format!(
                "Formal target: {}",
                session
                    .proof
                    .formal_target
                    .clone()
                    .unwrap_or_else(|| "none".to_string())
            ),
            format!(
                "Accepted target: {}",
                session
                    .proof
                    .accepted_target
                    .clone()
                    .unwrap_or_else(|| "none".to_string())
            ),
            format!(
                "Active node: {}",
                active_node
                    .map(|node| format!("{} [{}]", node.label, format_node_status(node.status)))
                    .unwrap_or_else(|| "none".to_string())
            ),
            format!(
                "Active branch: {}",
                active_branch
                    .map(|branch| format!("{} [{}]", branch.title, format_agent_status(branch.status)))
                    .unwrap_or_else(|| "none".to_string())
            ),
            format!("Proof nodes: {}", session.proof.nodes.len()),
            format!("Branches: {}", session.proof.branches.len()),
            format!("Agents: {}", session.proof.agents.len()),
            format!(
                "Paper notes: {}",
                session.proof.paper_notes.len()
            ),
            format!(
                "Pending question: {}",
                session
                    .proof
                    .pending_question
                    .as_ref()
                    .map(|item| item.prompt.clone())
                    .unwrap_or_else(|| "none".to_string())
            ),
            format!(
                "Awaiting clarification: {}",
                if session.proof.awaiting_clarification { "yes" } else { "no" }
            ),
            format!(
                "Autonomous: {}",
                if session.proof.is_autonomous_running {
                    format!(
                        "running (iteration {}, hidden {}, best {})",
                        session.proof.autonomous_iteration_count,
                        session.proof.hidden_branch_count,
                        session
                            .proof
                            .hidden_best_branch_id
                            .clone()
                            .unwrap_or_else(|| "none".to_string())
                    )
                } else {
                    session
                        .proof
                        .autonomous_pause_reason
                        .clone()
                        .or(session.proof.autonomous_stop_reason.clone())
                        .unwrap_or_else(|| "idle".to_string())
                }
            ),
            format!("Verified nodes: {}", verified),
            format!("Failed nodes: {}", failing),
            format!(
                "Assistant turn: {}",
                if self.turn_in_flight { "running" } else { "idle" }
            ),
            format!(
                "Lean verify: {}",
                if self.verification_in_flight {
                    "running"
                } else {
                    "idle"
                }
            ),
            format!(
                "Auth: {}",
                if self.auth.logged_in {
                    self.auth.email.clone().unwrap_or_else(|| "logged in".to_string())
                } else {
                    "logged out".to_string()
                }
            ),
            format!(
                "Lean: {}",
                if self.lean.ok {
                    self.lean
                        .lean_version
                        .clone()
                        .unwrap_or_else(|| "ok".to_string())
                } else {
                    "not ready".to_string()
                }
            ),
            format!(
                "Strategy: {}",
                session
                    .proof
                    .strategy_summary
                    .clone()
                    .unwrap_or_else(|| "none".to_string())
            ),
            format!(
                "Goal summary: {}",
                session
                    .proof
                    .goal_summary
                    .clone()
                    .unwrap_or_else(|| "none".to_string())
            ),
            format!(
                "Latest diagnostics: {}",
                session
                    .proof
                    .latest_diagnostics
                    .clone()
                    .unwrap_or_else(|| "none".to_string())
            ),
        ]
        .join("\n")
    }

    pub fn set_autonomous_run_state(
        &mut self,
        patch: AutonomousRunPatch,
    ) -> Result<PendingWrite, String> {
        let timestamp = Utc::now().to_rfc3339();
        let snapshot = {
            let Some(session) = self.current_session_mut() else {
                return Err("No active session.".to_string());
            };
            session.updated_at = timestamp;
            if let Some(value) = patch.is_autonomous_running {
                session.proof.is_autonomous_running = value;
            }
            if let Some(value) = patch.autonomous_iteration_count {
                session.proof.autonomous_iteration_count = value;
            }
            if let Some(value) = patch.autonomous_started_at {
                session.proof.autonomous_started_at = value;
            }
            if let Some(value) = patch.autonomous_last_progress_at {
                session.proof.autonomous_last_progress_at = value;
            }
            if let Some(value) = patch.autonomous_pause_reason {
                session.proof.autonomous_pause_reason = value;
            }
            if let Some(value) = patch.autonomous_stop_reason {
                session.proof.autonomous_stop_reason = value;
            }
            session.clone()
        };
        self.pending_writes += 1;
        self.status = if snapshot.proof.is_autonomous_running {
            "Autonomous proof loop is running.".to_string()
        } else {
            snapshot
                .proof
                .autonomous_pause_reason
                .clone()
                .or(snapshot.proof.autonomous_stop_reason.clone())
                .unwrap_or_else(|| "Autonomous proof loop is idle.".to_string())
        };
        Ok(PendingWrite { session: snapshot })
    }

    pub fn set_strategy_summary(&mut self, summary: Option<&str>) -> Result<PendingWrite, String> {
        let timestamp = Utc::now().to_rfc3339();
        let snapshot = {
            let Some(session) = self.current_session_mut() else {
                return Err("No active session.".to_string());
            };
            session.updated_at = timestamp;
            session.proof.strategy_summary = summary
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            session.clone()
        };
        self.pending_writes += 1;
        self.status = "Updated proof strategy summary.".to_string();
        Ok(PendingWrite { session: snapshot })
    }

    pub fn refresh_hidden_search_state(
        &mut self,
        active_retrieval_summary: Option<Option<String>>,
    ) -> Result<PendingWrite, String> {
        let timestamp = Utc::now().to_rfc3339();
        let snapshot = {
            let Some(session) = self.current_session_mut() else {
                return Err("No active session.".to_string());
            };
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
            session.updated_at = timestamp;
            session.proof.hidden_branch_count = hidden.len();
            session.proof.hidden_best_branch_id = hidden.first().map(|branch| branch.id.clone());
            if let Some(value) = active_retrieval_summary {
                session.proof.active_retrieval_summary = value;
            }
            session.clone()
        };
        self.pending_writes += 1;
        self.status = format!(
            "Hidden search state refreshed ({} hidden branches).",
            snapshot.proof.hidden_branch_count
        );
        Ok(PendingWrite { session: snapshot })
    }

    pub fn promote_branch_to_foreground(
        &mut self,
        branch_id: &str,
        resolved: bool,
        reason: Option<&str>,
    ) -> Result<PendingWrite, String> {
        let timestamp = Utc::now().to_rfc3339();
        let snapshot = {
            let Some(session) = self.current_session_mut() else {
                return Err("No active session.".to_string());
            };
            let previous_active = session.proof.active_foreground_branch_id.clone();
            if let Some(previous_id) = previous_active.as_deref().filter(|id| *id != branch_id) {
                if let Some(previous) = session
                    .proof
                    .branches
                    .iter_mut()
                    .find(|branch| branch.id == previous_id)
                {
                    previous.hidden = true;
                    previous.branch_kind = agent_role_label(previous.role).to_string();
                    previous.superseded_by_branch_id = Some(branch_id.to_string());
                    previous.updated_at = timestamp.clone();
                    if resolved && previous.status == AgentStatus::Running {
                        previous.status = AgentStatus::Done;
                        previous.queue_state = BranchQueueState::Done;
                    }
                    let suffix = format!("Superseded by {}.", branch_id);
                    previous.summary = if previous.summary.trim().is_empty() {
                        suffix
                    } else {
                        format!("{} {}", previous.summary.trim(), suffix)
                    };
                }
            }

            let branch = session
                .proof
                .branches
                .iter_mut()
                .find(|branch| branch.id == branch_id)
                .ok_or_else(|| format!("Branch not found: {branch_id}"))?;
            let promoted_from_hidden = branch.hidden || branch.promoted_from_hidden;
            branch.hidden = false;
            branch.branch_kind = "foreground".to_string();
            branch.promoted_from_hidden = promoted_from_hidden;
            branch.superseded_by_branch_id = None;
            branch.updated_at = timestamp.clone();
            if resolved {
                branch.status = AgentStatus::Done;
                branch.queue_state = BranchQueueState::Done;
                branch.phase = Some("done".to_string());
                branch.latest_diagnostics = None;
                branch.last_lean_diagnostic.clear();
            }
            let promoted_id = branch.id.clone();
            let promoted_role = branch.role;
            let promoted_focus_node_id = branch.focus_node_id.clone();
            let promoted_goal_summary = branch
                .latest_goals
                .clone()
                .or_else(|| (!branch.goal_summary.trim().is_empty()).then(|| branch.goal_summary.clone()));
            let promoted_latest_diagnostics = if resolved {
                None
            } else {
                branch
                    .latest_diagnostics
                    .clone()
                    .or_else(|| (!branch.last_lean_diagnostic.trim().is_empty()).then(|| branch.last_lean_diagnostic.clone()))
            };

            session.updated_at = timestamp.clone();
            session.proof.active_foreground_branch_id = Some(promoted_id);
            session.proof.active_branch_id = Some(branch_id.to_string());
            session.proof.active_agent_role = Some(promoted_role);
            if let Some(focus_node_id) = promoted_focus_node_id {
                session.proof.active_node_id = Some(focus_node_id);
            }
            session.proof.goal_summary = promoted_goal_summary;
            session.proof.latest_diagnostics = promoted_latest_diagnostics;
            if resolved {
                session.proof.resolved_by_branch_id = Some(branch_id.to_string());
                session.proof.phase = "done".to_string();
                session.proof.status_line = reason
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .unwrap_or("Lean verified the current theorem.")
                    .to_string();
            }

            let mut hidden = session
                .proof
                .branches
                .iter()
                .filter(|candidate| candidate.hidden)
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
            session.proof.hidden_best_branch_id = hidden.first().map(|item| item.id.clone());
            session.clone()
        };
        self.pending_writes += 1;
        self.status = snapshot.proof.status_line.clone();
        Ok(PendingWrite { session: snapshot })
    }

    pub fn create_session(&mut self, title: Option<&str>) -> PendingWrite {
        let mut session = default_session_with_workspace(
            self.workspace_root.as_deref(),
            self.workspace_label.as_deref(),
        );
        if let Some(title) = title {
            let trimmed = title.trim();
            if !trimmed.is_empty() {
                session.title = trimmed.to_string();
            }
        }
        self.sessions.insert(0, session.clone());
        self.selected_session = 0;
        self.scroll_offset = 0;
        self.flushed_turn_count = 0;
        self.selected_question_option = 0;
        self.pending_writes += 1;
        self.status = format!("Started session {}.", session.title);
        PendingWrite { session }
    }

    pub fn switch_session(&mut self, session_id: &str) -> Result<(), String> {
        let Some(index) = self.sessions.iter().position(|session| session.id == session_id) else {
            return Err(format!("Session not found: {session_id}"));
        };
        self.selected_session = index;
        self.scroll_offset = 0;
        self.flushed_turn_count = 0;
        self.sync_question_selection();
        let title = self.sessions[index].title.clone();
        self.status = format!("Resumed {title}.");
        Ok(())
    }

    pub fn set_share_mode(&mut self, share_mode: ShareMode) -> Result<PendingWrite, String> {
        let timestamp = Utc::now().to_rfc3339();
        let snapshot = {
            let Some(session) = self.current_session_mut() else {
                return Err("No active session.".to_string());
            };
            session.updated_at = timestamp;
            session.cloud.share_mode = share_mode;
            if share_mode == ShareMode::Local {
                session.cloud.sync_enabled = false;
            }
            session.clone()
        };
        self.pending_writes += 1;
        self.status = format!("Share mode set to {}.", share_mode_label(share_mode));
        Ok(PendingWrite { session: snapshot })
    }

    pub fn set_sync_enabled(&mut self, enabled: bool) -> Result<PendingWrite, String> {
        let timestamp = Utc::now().to_rfc3339();
        let snapshot = {
            let Some(session) = self.current_session_mut() else {
                return Err("No active session.".to_string());
            };
            if session.cloud.share_mode == ShareMode::Local && enabled {
                return Err("Set share mode to community or private before enabling sync.".to_string());
            }
            session.updated_at = timestamp;
            session.cloud.sync_enabled = enabled;
            session.clone()
        };
        self.pending_writes += 1;
        self.status = if enabled {
            "Enabled sync for the current session.".to_string()
        } else {
            "Disabled sync for the current session.".to_string()
        };
        Ok(PendingWrite { session: snapshot })
    }

    pub fn set_private_overlay_community(&mut self, enabled: bool) -> Result<PendingWrite, String> {
        let timestamp = Utc::now().to_rfc3339();
        let snapshot = {
            let Some(session) = self.current_session_mut() else {
                return Err("No active session.".to_string());
            };
            session.updated_at = timestamp;
            session.cloud.private_overlay_community = enabled;
            session.clone()
        };
        self.pending_writes += 1;
        self.status = if enabled {
            "Private share mode will also search the community overlay.".to_string()
        } else {
            "Private share mode will stay isolated from community results.".to_string()
        };
        Ok(PendingWrite { session: snapshot })
    }

    pub fn mark_sync_completed(&mut self) -> Result<PendingWrite, String> {
        let timestamp = Utc::now().to_rfc3339();
        let snapshot = {
            let Some(session) = self.current_session_mut() else {
                return Err("No active session.".to_string());
            };
            session.updated_at = timestamp.clone();
            session.cloud.last_sync_at = Some(timestamp);
            session.clone()
        };
        self.pending_writes += 1;
        self.status = "Shared corpus sync completed.".to_string();
        Ok(PendingWrite { session: snapshot })
    }

    pub fn proof_nodes_report(&self) -> String {
        let Some(session) = self.current_session() else {
            return "No active session.".to_string();
        };
        if session.proof.nodes.is_empty() {
            return "No proof nodes yet.".to_string();
        }
        session
            .proof
            .nodes
            .iter()
            .map(|node| {
                let focused = session
                    .proof
                    .active_node_id
                    .as_ref()
                    .map(|active| active == &node.id)
                    .unwrap_or(false);
                format!(
                    "{}{}  {}  {} :: {}",
                    if focused { "* " } else { "" },
                    node.id,
                    format_node_status(node.status),
                    node.label,
                    node.statement
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn branches_report(&self) -> String {
        let Some(session) = self.current_session() else {
            return "No active session.".to_string();
        };
        if session.proof.branches.is_empty() {
            return "No branches yet.".to_string();
        }
        session
            .proof
            .branches
            .iter()
            .map(|branch| {
                let focused = session
                    .proof
                    .active_branch_id
                    .as_ref()
                    .map(|active| active == &branch.id)
                    .unwrap_or(false);
                format!(
                    "{}{}  {}  {}  {}",
                    if focused { "* " } else { "" },
                    branch.id,
                    format_agent_status(branch.status),
                    agent_role_label(branch.role),
                    branch.title
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn agents_report(&self) -> String {
        let Some(session) = self.current_session() else {
            return "No active session.".to_string();
        };
        if session.proof.agents.is_empty() {
            return "No agents configured.".to_string();
        }
        session
            .proof
            .agents
            .iter()
            .map(|agent| {
                let task = agent
                    .tasks
                    .last()
                    .map(|task| format!(" · {}", task.title))
                    .unwrap_or_default();
                format!(
                    "{}  {}{}",
                    agent_role_label(agent.role),
                    format_agent_status(agent.status),
                    task
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn tasks_report(&self) -> String {
        let Some(session) = self.current_session() else {
            return "No active session.".to_string();
        };
        let tasks = session
            .proof
            .agents
            .iter()
            .flat_map(|agent| agent.tasks.iter())
            .map(|task| {
                format!(
                    "{}  {}  {}  {}",
                    task.id,
                    agent_role_label(task.role),
                    format_agent_status(task.status),
                    task.title
                )
            })
            .collect::<Vec<_>>();
        if tasks.is_empty() {
            "No tasks yet.".to_string()
        } else {
            tasks.join("\n")
        }
    }

    pub fn spawn_agent_branch(
        &mut self,
        role: AgentRole,
        title: &str,
        description: &str,
        hidden: bool,
    ) -> Result<(PendingWrite, String, String), String> {
        let title = title.trim();
        if title.is_empty() {
            return Err("Agent title cannot be empty.".to_string());
        }
        let timestamp = Utc::now().to_rfc3339();
        let branch_id = next_id("branch");
        let task_id = next_id("task");
        let agent_id = next_id("agent");
        let snapshot = {
            let Some(session) = self.current_session_mut() else {
                return Err("No active session.".to_string());
            };
            let branch = ProofBranch {
                id: branch_id.clone(),
                role,
                title: title.to_string(),
                branch_kind: if hidden {
                    format!("{}_hidden", agent_role_label(role))
                } else {
                    agent_role_label(role).to_string()
                },
                hidden,
                status: AgentStatus::Running,
                phase: Some(phase_from_role(role).to_string()),
                queue_state: BranchQueueState::Running,
                task_id: Some(task_id.clone()),
                focus_node_id: session.proof.active_node_id.clone(),
                goal_summary: description.trim().to_string(),
                score: 0.0,
                attempt_count: 0,
                progress_kind: Some(
                    match role {
                        AgentRole::Planner => "planning",
                        AgentRole::Retriever => "retrieving",
                        AgentRole::Repairer => "repairing",
                        AgentRole::Prover => "candidate",
                        AgentRole::Critic => "blocked",
                    }
                    .to_string(),
                ),
                last_lean_diagnostic: String::new(),
                latest_diagnostics: None,
                latest_goals: None,
                last_successful_check_at: None,
                search_status: format!("{} branch started", agent_role_label(role)),
                lean_snippet: String::new(),
                diagnostics: String::new(),
                summary: description.trim().to_string(),
                promoted_from_hidden: false,
                superseded_by_branch_id: None,
                transcript: vec![BranchMessage {
                    id: next_id("branchmsg"),
                    role: MessageRole::System,
                    content: format!("Started {} branch: {}", agent_role_label(role), title),
                    created_at: timestamp.clone(),
                }],
                created_at: timestamp.clone(),
                updated_at: timestamp.clone(),
            };
            let task = AgentTask {
                id: task_id.clone(),
                role,
                title: title.to_string(),
                status: AgentStatus::Running,
                description: description.trim().to_string(),
                branch_id: Some(branch_id.clone()),
                output: String::new(),
                created_at: timestamp.clone(),
                updated_at: timestamp.clone(),
            };
            let agent = AgentRecord {
                id: agent_id,
                role,
                status: AgentStatus::Running,
                title: title.to_string(),
                tasks: vec![task],
                current_task_id: Some(task_id.clone()),
                branch_ids: vec![branch_id.clone()],
                updated_at: timestamp.clone(),
            };
            session.updated_at = timestamp;
            session.proof.active_agent_role = Some(role);
            session.proof.active_branch_id = Some(branch_id.clone());
            if role == AgentRole::Prover && !hidden {
                session.proof.active_foreground_branch_id = Some(branch_id.clone());
            }
            if hidden {
                session.proof.hidden_best_branch_id = Some(branch_id.clone());
            }
            session.proof.hidden_branch_count = session.proof.branches.iter().filter(|branch| branch.hidden).count()
                + if hidden { 1 } else { 0 };
            session.proof.phase = phase_from_role(role).to_string();
            session.proof.status_line = format!("{} branch running: {}", agent_role_label(role), title);
            session.proof.branches.push(branch);
            session.proof.agents.push(agent);
            session.clone()
        };
        self.pending_writes += 1;
        self.status = format!("Started {} branch {}.", agent_role_label(role), title);
        Ok((PendingWrite { session: snapshot }, branch_id, task_id))
    }

    pub fn focus_target(&mut self, target: Option<&str>) -> Result<Option<PendingWrite>, String> {
        let Some(target) = target.filter(|value| !value.trim().is_empty()) else {
            let timestamp = Utc::now().to_rfc3339();
            let snapshot = {
                let Some(session) = self.current_session_mut() else {
                    return Err("No active session.".to_string());
                };
                session.updated_at = timestamp;
                session.proof.active_branch_id = None;
                session.proof.active_node_id = None;
                session.clone()
            };
            self.pending_writes += 1;
            self.status = "Cleared active focus.".to_string();
            return Ok(Some(PendingWrite { session: snapshot }));
        };
        if target.starts_with("branch_") {
            let timestamp = Utc::now().to_rfc3339();
            let snapshot = {
                let Some(session) = self.current_session_mut() else {
                    return Err("No active session.".to_string());
                };
                let branch = session
                    .proof
                    .branches
                    .iter()
                    .find(|branch| branch.id == target)
                    .ok_or_else(|| format!("Branch not found: {target}"))?
                    .clone();
                session.updated_at = timestamp;
                session.proof.active_branch_id = Some(branch.id.clone());
                session.proof.active_agent_role = Some(branch.role);
                if let Some(focus_node_id) = branch.focus_node_id.clone() {
                    session.proof.active_node_id = Some(focus_node_id);
                }
                session.proof.status_line = format!("Focused branch {}.", branch.title);
                session.clone()
            };
            self.pending_writes += 1;
            self.status = format!("Focused branch {target}.");
            return Ok(Some(PendingWrite { session: snapshot }));
        }
        self.set_active_node(Some(target))
    }

    pub fn paper_report(&self) -> String {
        let Some(session) = self.current_session() else {
            return "No active session.".to_string();
        };
        let mut lines = vec![
            format!("Title: {}", session.title),
            format!(
                "Problem: {}",
                session
                    .proof
                    .problem
                    .clone()
                    .unwrap_or_else(|| "none".to_string())
            ),
            format!(
                "Formal target: {}",
                session
                    .proof
                    .formal_target
                    .clone()
                    .unwrap_or_else(|| "none".to_string())
            ),
            format!(
                "Accepted target: {}",
                session
                    .proof
                    .accepted_target
                    .clone()
                    .unwrap_or_else(|| "none".to_string())
            ),
            String::new(),
            "Notes:".to_string(),
        ];
        if session.proof.paper_notes.is_empty() {
            lines.push("No paper notes yet.".to_string());
        } else {
            lines.extend(
                session
                    .proof
                    .paper_notes
                    .iter()
                    .enumerate()
                    .map(|(index, note)| format!("{}. {}", index + 1, note)),
            );
        }
        lines.join("\n")
    }

    pub fn pending_question_report(&self) -> String {
        let Some(session) = self.current_session() else {
            return "No active session.".to_string();
        };
        let Some(question) = &session.proof.pending_question else {
            return "No pending clarification question.".to_string();
        };
        let mut lines = vec![
            question.prompt.clone(),
            format!("Status: {}", question.status),
        ];
        if let Some(answer) = &question.answer_text {
            lines.push(format!("Answer: {answer}"));
        }
        if question.options.is_empty() {
            lines.push("No options recorded.".to_string());
        } else {
            lines.push(String::new());
            lines.extend(question.options.iter().map(|option| {
                let recommended = question
                    .recommended_option_id
                    .as_ref()
                    .map(|value| value == &option.id)
                    .unwrap_or(false);
                let mut line = format!("{}: {}", option.id, option.label);
                if recommended {
                    line.push_str(" [recommended]");
                }
                if !option.summary.trim().is_empty() {
                    line.push_str(&format!(" — {}", option.summary.trim()));
                }
                if !option.formal_target.trim().is_empty() {
                    line.push_str(&format!("\n  {}", option.formal_target.trim()));
                }
                line
            }));
        }
        lines.join("\n")
    }

    pub fn proof_status_report(&self) -> String {
        let Some(session) = self.current_session() else {
            return "No active session.".to_string();
        };
        let mut lines = vec![
            format!("Phase: {}", session.proof.phase),
            format!("Status: {}", session.proof.status_line),
        ];
        if let Some(problem) = &session.proof.problem {
            lines.push(format!("Problem: {problem}"));
        }
        if let Some(target) = &session.proof.formal_target {
            lines.push(format!("Formal target: {target}"));
        }
        if let Some(target) = &session.proof.accepted_target {
            lines.push(format!("Accepted target: {target}"));
        }
        if session.proof.nodes.is_empty() {
            lines.push("No proof nodes.".to_string());
        } else {
            lines.push(String::new());
            lines.push(format!("Nodes ({}):", session.proof.nodes.len()));
            for node in &session.proof.nodes {
                lines.push(format!(
                    "  {} [{}] :: {}",
                    node.label,
                    format_node_status(node.status),
                    node.statement
                ));
            }
        }
        if !session.proof.branches.is_empty() {
            lines.push(String::new());
            lines.push(format!("Branches ({}):", session.proof.branches.len()));
            for branch in session.proof.branches.iter().rev().take(6).rev() {
                lines.push(format!(
                    "  {} [{}] {}",
                    agent_role_label(branch.role),
                    format_agent_status(branch.status),
                    branch.title
                ));
            }
        }
        if let Some(last) = &session.proof.last_verification {
            lines.push(String::new());
            if last.ok {
                lines.push("Last verification: OK".to_string());
            } else {
                lines.push(format!(
                    "Last verification: FAILED -- {}",
                    last.stderr.lines().next().unwrap_or("error")
                ));
            }
        }
        lines.join("\n")
    }

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
                if let Some(snapshot) = self.current_session_mut().and_then(|session| {
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
                    Some(session.clone())
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
                            let node = ProofNode {
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
                                let node = ProofNode {
                                    id: next_id("node"),
                                    kind: ProofNodeKind::Theorem,
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

/// Build focusable targets (nodes + branches) for the focus picker.
pub fn build_focus_items(state: &AppState) -> Vec<(String, String, String)> {
    let mut items = Vec::new();
    if let Some(session) = state.current_session() {
        for node in &session.proof.nodes {
            let kind = format!("{:?}", node.kind).to_lowercase();
            items.push((node.id.clone(), node.label.clone(), kind));
        }
        for branch in &session.proof.branches {
            let kind = format!("branch/{}", branch.branch_kind);
            items.push((branch.id.clone(), branch.title.clone(), kind));
        }
    }
    items
}

/// All known slash commands for tab completion.
pub const SLASH_COMMANDS: &[&str] = &[
    "help",
    "status",
    "new",
    "clear",
    "resume",
    "nodes",
    "branches",
    "agents",
    "tasks",
    "focus",
    "agent spawn",
    "proof",
    "paper",
    "questions",
    "answer",
    "instructions",
    "memory",
    "remember",
    "share",
    "corpus status",
    "corpus search",
    "corpus ingest",
    "corpus recluster",
    "sync status",
    "sync enable",
    "sync disable",
    "sync drain",
    "export paper",
    "export tex",
    "export lean",
    "export all",
    "autonomous status",
    "autonomous start",
    "autonomous stop",
    "autonomous step",
    "theorem",
    "lemma",
    "verify",
    "login",
    "dashboard",
    "sessions",
];

/// Compute tab completions for the current command buffer.
pub fn command_completions(input: &str) -> Vec<String> {
    SLASH_COMMANDS
        .iter()
        .filter(|c| c.starts_with(input))
        .map(|c| c.to_string())
        .collect()
}

/// Find the byte position after deleting one word backward from `cursor`.
///
/// Skips trailing whitespace, then skips the word, returning the byte offset
/// of the start of the word. Used for Ctrl+W / Alt+Backspace handling.
pub fn delete_word_backward_pos(text: &str, cursor: usize) -> usize {
    text[..cursor]
        .char_indices()
        .rev()
        .skip_while(|(_, c)| c.is_whitespace())
        .skip_while(|(_, c)| !c.is_whitespace())
        .map(|(i, c)| i + c.len_utf8())
        .next()
        .unwrap_or(0)
}

fn default_session_with_workspace(
    workspace_root: Option<&str>,
    workspace_label: Option<&str>,
) -> SessionSnapshot {
    let timestamp = Utc::now().to_rfc3339();
    SessionSnapshot {
        id: next_id("rust_session"),
        title: "OpenProof Rust Session".to_string(),
        updated_at: timestamp,
        workspace_root: workspace_root.map(str::to_string),
        workspace_label: workspace_label.map(str::to_string),
        cloud: CloudPolicy::default(),
        transcript: Vec::new(),
        proof: ProofSessionState {
            phase: "idle".to_string(),
            status_line: "Ready.".to_string(),
            root_node_id: None,
            problem: None,
            formal_target: None,
            accepted_target: None,
            search_status: None,
            assumptions: Vec::new(),
            paper_notes: Vec::new(),
            pending_question: None,
            awaiting_clarification: false,
            is_autonomous_running: false,
            autonomous_iteration_count: 0,
            autonomous_started_at: None,
            autonomous_last_progress_at: None,
            autonomous_pause_reason: None,
            autonomous_stop_reason: None,
            hidden_best_branch_id: None,
            active_retrieval_summary: None,
            strategy_summary: None,
            goal_summary: None,
            latest_diagnostics: None,
            active_node_id: None,
            active_branch_id: None,
            active_agent_role: None,
            active_foreground_branch_id: None,
            resolved_by_branch_id: None,
            hidden_branch_count: 0,
            imports: vec!["Mathlib".to_string()],
            nodes: Vec::new(),
            branches: Vec::new(),
            agents: Vec::new(),
            last_rendered_scratch: None,
            last_verification: None,
            paper_tex: String::new(),
            scratch_path: None,
            paper_path: None,
            attempt_number: 0,
        },
    }
}

fn next_id(prefix: &str) -> String {
    format!("{prefix}_{}", Utc::now().timestamp_millis())
}

fn format_node_status(status: ProofNodeStatus) -> &'static str {
    match status {
        ProofNodeStatus::Pending => "pending",
        ProofNodeStatus::Suggested => "suggested",
        ProofNodeStatus::Proving => "proving",
        ProofNodeStatus::Verifying => "verifying",
        ProofNodeStatus::Verified => "verified",
        ProofNodeStatus::Failed => "failed",
        ProofNodeStatus::Abandoned => "abandoned",
    }
}

fn share_mode_label(mode: ShareMode) -> &'static str {
    match mode {
        ShareMode::Local => "local",
        ShareMode::Community => "community",
        ShareMode::Private => "private",
    }
}

fn phase_from_role(role: AgentRole) -> &'static str {
    match role {
        AgentRole::Planner => "planning",
        AgentRole::Retriever => "retrieving",
        AgentRole::Prover => "proving",
        AgentRole::Repairer => "repairing",
        AgentRole::Critic => "blocked",
    }
}

fn agent_role_label(role: AgentRole) -> &'static str {
    match role {
        AgentRole::Planner => "planner",
        AgentRole::Prover => "prover",
        AgentRole::Repairer => "repairer",
        AgentRole::Retriever => "retriever",
        AgentRole::Critic => "critic",
    }
}

fn format_agent_status(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Idle => "idle",
        AgentStatus::Running => "running",
        AgentStatus::Blocked => "blocked",
        AgentStatus::Done => "done",
        AgentStatus::Error => "error",
    }
}

fn extract_lean_code_block(content: &str) -> Option<String> {
    let start = content.find("```lean")?;
    let rest = &content[start + "```lean".len()..];
    let rest = rest.strip_prefix('\n').unwrap_or(rest);
    let end = rest.find("```")?;
    let block = rest[..end].trim();
    if block.is_empty() {
        None
    } else {
        Some(block.to_string())
    }
}

fn summarize_lean_error(result: &LeanVerificationSummary) -> String {
    let message = if !result.stderr.trim().is_empty() {
        result.stderr.trim()
    } else if let Some(error) = result.error.as_deref() {
        error.trim()
    } else {
        "Lean verification failed."
    };
    message
        .lines()
        .take(12)
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Debug, Clone)]
struct ParsedAssistantNode {
    kind: ProofNodeKind,
    label: String,
    statement: String,
}

#[derive(Debug, Clone, Default)]
struct ParsedAssistantOutput {
    title: Option<String>,
    problem: Option<String>,
    formal_target: Option<String>,
    accepted_target: Option<String>,
    phase: Option<String>,
    search_status: Option<String>,
    assumptions: Vec<String>,
    paper_notes: Vec<String>,
    paper_tex: Option<String>,
    next_steps: Vec<String>,
    lean_snippets: Vec<String>,
    created_nodes: Vec<ParsedAssistantNode>,
    question: Option<ProofQuestionState>,
}

fn parse_assistant_output(text: &str) -> ParsedAssistantOutput {
    let mut parsed = ParsedAssistantOutput {
        lean_snippets: extract_lean_code_blocks(text),
        paper_tex: extract_latex_block(text),
        ..ParsedAssistantOutput::default()
    };
    let mut question_prompt: Option<String> = None;
    let mut question_options: Vec<ProofQuestionOption> = Vec::new();
    let mut recommended_option_id: Option<String> = None;

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(value) = strip_prefix_case_insensitive(line, "TITLE:") {
            parsed.title = Some(value.to_string());
            continue;
        }
        if let Some(value) = strip_prefix_case_insensitive(line, "PROBLEM:") {
            parsed.problem = Some(value.to_string());
            continue;
        }
        if let Some(value) = strip_prefix_case_insensitive(line, "FORMAL_TARGET:") {
            parsed.formal_target = Some(value.to_string());
            continue;
        }
        if let Some(value) = strip_prefix_case_insensitive(line, "ACCEPTED_TARGET:") {
            parsed.accepted_target = Some(value.to_string());
            continue;
        }
        if let Some(value) = strip_prefix_case_insensitive(line, "PHASE:") {
            parsed.phase = Some(value.to_lowercase());
            continue;
        }
        if let Some(value) = strip_prefix_case_insensitive(line, "SEARCH:") {
            parsed.search_status = Some(value.to_string());
            continue;
        }
        if let Some(value) = strip_prefix_case_insensitive(line, "STATUS:") {
            parsed.search_status = Some(value.to_string());
            continue;
        }
        if let Some(value) = strip_prefix_case_insensitive(line, "QUESTION:") {
            question_prompt = Some(value.to_string());
            continue;
        }
        if let Some(value) = strip_prefix_case_insensitive(line, "RECOMMENDED_OPTION:") {
            recommended_option_id = Some(value.to_string());
            continue;
        }
        if let Some(value) = strip_prefix_case_insensitive(line, "ASSUMPTION:") {
            parsed.assumptions.push(value.to_string());
            continue;
        }
        if let Some(value) = strip_prefix_case_insensitive(line, "PAPER:") {
            parsed.paper_notes.push(value.to_string());
            continue;
        }
        if let Some(value) = strip_prefix_case_insensitive(line, "PAPER_NOTE:") {
            parsed.paper_notes.push(value.to_string());
            continue;
        }
        if let Some(value) = strip_prefix_case_insensitive(line, "NEXT:") {
            parsed.next_steps.push(value.to_string());
            continue;
        }
        if let Some((label, statement)) = parse_labeled_statement(line, "THEOREM:") {
            parsed.created_nodes.push(ParsedAssistantNode {
                kind: ProofNodeKind::Theorem,
                label,
                statement,
            });
            continue;
        }
        if let Some((label, statement)) = parse_labeled_statement(line, "LEMMA:") {
            parsed.created_nodes.push(ParsedAssistantNode {
                kind: ProofNodeKind::Lemma,
                label,
                statement,
            });
            continue;
        }
        if let Some((label, statement)) = parse_labeled_statement(line, "LEMMA_CANDIDATE:") {
            parsed.created_nodes.push(ParsedAssistantNode {
                kind: ProofNodeKind::Lemma,
                label,
                statement,
            });
            continue;
        }
        if let Some(option) = parse_question_option(line) {
            question_options.push(option);
            continue;
        }
        if let Some((option_id, target)) = parse_option_target(line) {
            if let Some(existing) = question_options.iter_mut().find(|option| option.id == option_id) {
                existing.formal_target = target;
            } else {
                question_options.push(ProofQuestionOption {
                    id: option_id.clone(),
                    label: option_id,
                    summary: String::new(),
                    formal_target: target,
                });
            }
        }
    }

    if let Some(prompt) = question_prompt.filter(|item| !item.trim().is_empty()) {
        let options = question_options
            .into_iter()
            .filter(|option| !option.formal_target.trim().is_empty())
            .collect::<Vec<_>>();
        if !options.is_empty() {
            parsed.question = Some(ProofQuestionState {
                prompt,
                options,
                recommended_option_id,
                answer_text: None,
                status: "open".to_string(),
            });
        }
    }

    parsed
}

fn strip_prefix_case_insensitive<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let line_upper = line.to_ascii_uppercase();
    let prefix_upper = prefix.to_ascii_uppercase();
    if line_upper.starts_with(&prefix_upper) {
        Some(line[prefix.len()..].trim())
    } else {
        None
    }
}

fn parse_labeled_statement(line: &str, prefix: &str) -> Option<(String, String)> {
    let body = strip_prefix_case_insensitive(line, prefix)?;
    let (label, statement) = body.split_once("::")?;
    let label = label.trim();
    let statement = statement.trim();
    if label.is_empty() || statement.is_empty() {
        None
    } else {
        Some((label.to_string(), statement.to_string()))
    }
}

fn parse_question_option(line: &str) -> Option<ProofQuestionOption> {
    let body = strip_prefix_case_insensitive(line, "OPTION:")?;
    let parts = body.split('|').map(str::trim).collect::<Vec<_>>();
    if parts.len() < 2 {
        return None;
    }
    let id = parts[0];
    let label = parts[1];
    if id.is_empty() || label.is_empty() {
        return None;
    }
    Some(ProofQuestionOption {
        id: id.to_string(),
        label: label.to_string(),
        summary: parts.get(2).copied().unwrap_or_default().to_string(),
        formal_target: String::new(),
    })
}

fn parse_option_target(line: &str) -> Option<(String, String)> {
    let body = strip_prefix_case_insensitive(line, "OPTION_TARGET:")
        .or_else(|| strip_prefix_case_insensitive(line, "FORMAL_TARGET_OPTION:"))?;
    let (id, target) = body.split_once("::")?;
    let id = id.trim();
    let target = target.trim();
    if id.is_empty() || target.is_empty() {
        None
    } else {
        Some((id.to_string(), target.to_string()))
    }
}

fn extract_lean_code_blocks(content: &str) -> Vec<String> {
    let mut snippets = Vec::new();
    let mut rest = content;
    while let Some(start) = rest.find("```lean") {
        let after_start = &rest[start + "```lean".len()..];
        let after_start = after_start.strip_prefix('\n').unwrap_or(after_start);
        let Some(end) = after_start.find("```") else {
            break;
        };
        let block = after_start[..end].trim();
        if !block.is_empty() {
            snippets.push(block.to_string());
        }
        rest = &after_start[end + "```".len()..];
    }
    snippets
}

fn extract_latex_block(content: &str) -> Option<String> {
    let start = content.find("```latex")?;
    let after_start = &content[start + "```latex".len()..];
    let after_start = after_start.strip_prefix('\n').unwrap_or(after_start);
    let end = after_start.find("```")?;
    let block = after_start[..end].trim();
    if block.is_empty() {
        None
    } else {
        Some(block.to_string())
    }
}

fn derive_goal_label(title: &str) -> String {
    let mut label = title
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect::<String>();
    while label.contains("__") {
        label = label.replace("__", "_");
    }
    label = label.trim_matches('_').to_string();
    if label.is_empty() {
        "goal".to_string()
    } else if label.chars().next().map(|ch| ch.is_ascii_digit()).unwrap_or(false) {
        format!("goal_{label}")
    } else {
        label
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_structured_assistant_output() {
        let parsed = parse_assistant_output(
            r#"
TITLE: Prime Gap Goal
PROBLEM: Show a normalized prime-gap subsequence limit exists.
FORMAL_TARGET: ∀ C : ℝ, 0 ≤ C → True
ACCEPTED_TARGET: ∀ C : ℝ, 0 ≤ C → True
PHASE: proving
STATUS: searching local lemmas
ASSUMPTION: C ≥ 0
QUESTION: Which normalization should we use?
OPTION: A | Log normalization | standard asymptotic target
OPTION_TARGET: A :: ∀ C : ℝ, 0 ≤ C → True
RECOMMENDED_OPTION: A
THEOREM: PrimeGapTarget :: ∀ C : ℝ, 0 ≤ C → True
LEMMA: helper_limit :: True
PAPER: We normalize by log n.
NEXT: verify the current candidate
```lean
theorem PrimeGapTarget : ∀ C : ℝ, 0 ≤ C → True := by
  intro C hC
  trivial
```
"#,
        );

        assert_eq!(parsed.title.as_deref(), Some("Prime Gap Goal"));
        assert_eq!(
            parsed.accepted_target.as_deref(),
            Some("∀ C : ℝ, 0 ≤ C → True")
        );
        assert_eq!(parsed.phase.as_deref(), Some("proving"));
        assert_eq!(parsed.created_nodes.len(), 2);
        assert_eq!(parsed.paper_notes.len(), 1);
        assert_eq!(parsed.lean_snippets.len(), 1);
        assert!(parsed.question.is_some());
        let question = parsed.question.unwrap();
        assert_eq!(question.prompt, "Which normalization should we use?");
        assert_eq!(question.options.len(), 1);
        assert_eq!(question.recommended_option_id.as_deref(), Some("A"));
    }

    #[test]
    fn append_assistant_updates_proof_state() {
        let mut state = AppState::new(
            vec![default_session_with_workspace(None, Some("openproof"))],
            "ready".to_string(),
            None,
            Some("openproof".to_string()),
        );
        let write = state.add_proof_node(
            ProofNodeKind::Theorem,
            "PrimeGapTarget",
            "∀ C : ℝ, 0 ≤ C → True",
        );
        assert!(write.is_ok());
        let _ = state.apply(AppEvent::AppendAssistant(
            r#"
TITLE: Prime Gap Goal
FORMAL_TARGET: ∀ C : ℝ, 0 ≤ C → True
ACCEPTED_TARGET: ∀ C : ℝ, 0 ≤ C → True
PAPER: We normalize by log n.
QUESTION: Which normalization should we use?
OPTION: A | Log normalization | standard asymptotic target
OPTION_TARGET: A :: ∀ C : ℝ, 0 ≤ C → True
RECOMMENDED_OPTION: A
```lean
theorem PrimeGapTarget : ∀ C : ℝ, 0 ≤ C → True := by
  intro C hC
  trivial
```
"#
            .to_string(),
        ));

        let session = state.current_session().unwrap();
        assert_eq!(session.title, "Prime Gap Goal");
        assert_eq!(
            session.proof.formal_target.as_deref(),
            Some("∀ C : ℝ, 0 ≤ C → True")
        );
        assert_eq!(
            session.proof.accepted_target.as_deref(),
            Some("∀ C : ℝ, 0 ≤ C → True")
        );
        assert_eq!(session.proof.paper_notes.len(), 1);
        assert_eq!(
            session.proof.nodes.first().map(|node| node.content.contains("theorem PrimeGapTarget")),
            Some(true)
        );
    }

    #[test]
    fn question_selection_prefers_recommended_option() {
        let mut state = AppState::new(
            vec![default_session_with_workspace(None, Some("openproof"))],
            "ready".to_string(),
            None,
            Some("openproof".to_string()),
        );
        let _ = state.apply(AppEvent::AppendAssistant(
            r#"
QUESTION: Which target should we accept?
OPTION: A | Weak target | easier
OPTION_TARGET: A :: True
OPTION: B | Strong target | preferred
OPTION_TARGET: B :: ∀ n : ℕ, True
RECOMMENDED_OPTION: B
"#
            .to_string(),
        ));

        assert!(state.has_open_question());
        assert_eq!(
            state.selected_question_option().map(|option| option.id.as_str()),
            Some("B")
        );
    }
}
