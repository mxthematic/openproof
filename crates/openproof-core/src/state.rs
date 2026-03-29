use openproof_protocol::{
    AgentStatus, AuthSummary, LeanHealth, LeanVerificationSummary, SessionSnapshot, TranscriptEntry,
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
    AppendBranchAssistant {
        branch_id: String,
        content: String,
        used_tools: bool,
    },
    FinishBranch {
        branch_id: String,
        status: AgentStatus,
        summary: String,
        output: String,
    },
    AutonomousTick,
    AppendNotice {
        title: String,
        content: String,
    },
    ToolCallReceived {
        call_id: String,
        tool_name: String,
        arguments: String,
    },
    ToolResultReceived {
        call_id: String,
        tool_name: String,
        success: bool,
        output: String,
    },
    ToolLoopIteration(usize),
    /// Sync workspace file content into the active proof node.
    /// Emitted after a tool-using turn so node.content reflects what tools wrote.
    WorkspaceContentSync {
        content: String,
        verified: bool,
    },
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
    /// Update proof goal state (from Pantograph/tactic search).
    ProofGoalUpdated(openproof_protocol::ProofGoal),
    /// Tactic search completed for a sorry position.
    TacticSearchComplete {
        node_id: String,
        sorry_line: usize,
        solved: bool,
        tactics: Vec<String>,
        /// Number of remaining unsolved goals (None if solved).
        remaining_goals: Option<usize>,
        /// Number of tactic expansions tried.
        expansions: Option<usize>,
        /// Search outcome: "solved", "partial", "exhausted", "timeout".
        search_outcome: String,
    },
    /// Progress update from tactic search.
    TacticSearchProgress {
        node_id: String,
        sorry_line: usize,
        expansions: usize,
        best_remaining_goals: usize,
    },
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
    /// Input history (shell-style Up/Down navigation).
    pub input_history: Vec<String>,
    /// Current position in input history (None = not browsing).
    pub history_index: Option<usize>,
    /// Draft input saved when entering history browse mode.
    pub input_draft: String,
    /// Content of collapsed paste blocks. The Nth `\u{FFFC}` in `composer`
    /// corresponds to `paste_blocks[N]`.
    pub paste_blocks: Vec<String>,
    /// Whether the tool loop is currently active (executing tool calls).
    pub tool_loop_active: bool,
    /// Current iteration in the tool loop.
    pub tool_loop_iteration: usize,
    /// Name of the tool currently being executed (for status bar).
    pub current_tool_name: Option<String>,
    /// Human-readable activity description for the status bar.
    pub activity_label: String,
    /// When the current activity phase started.
    pub activity_started_at: Option<std::time::Instant>,
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
            input_history: Vec::new(),
            history_index: None,
            input_draft: String::new(),
            paste_blocks: Vec::new(),
            tool_loop_active: false,
            tool_loop_iteration: 0,
            current_tool_name: None,
            activity_label: String::new(),
            activity_started_at: None,
        }
    }
}
