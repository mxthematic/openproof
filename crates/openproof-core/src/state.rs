use openproof_protocol::{
    AuthSummary, LeanHealth, LeanVerificationSummary, AgentStatus, SessionSnapshot,
    TranscriptEntry,
};

use crate::helpers::default_session_with_workspace;

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
}
