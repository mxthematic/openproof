use chrono::Utc;
use openproof_protocol::{
    AgentRole, AgentStatus, CloudPolicy, LeanVerificationSummary, ProofNodeStatus,
    ProofSessionState, SessionSnapshot, ShareMode,
};

pub fn next_id(prefix: &str) -> String {
    format!("{prefix}_{}", Utc::now().timestamp_millis())
}

pub fn format_node_status(status: ProofNodeStatus) -> &'static str {
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

pub fn share_mode_label(mode: ShareMode) -> &'static str {
    match mode {
        ShareMode::Local => "local",
        ShareMode::Community => "community",
        ShareMode::Private => "private",
    }
}

pub fn phase_from_role(role: AgentRole) -> &'static str {
    match role {
        AgentRole::Planner => "planning",
        AgentRole::Retriever => "retrieving",
        AgentRole::Prover => "proving",
        AgentRole::Repairer => "repairing",
        AgentRole::Critic => "blocked",
    }
}

pub fn agent_role_label(role: AgentRole) -> &'static str {
    match role {
        AgentRole::Planner => "planner",
        AgentRole::Prover => "prover",
        AgentRole::Repairer => "repairer",
        AgentRole::Retriever => "retriever",
        AgentRole::Critic => "critic",
    }
}

pub fn format_agent_status(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Idle => "idle",
        AgentStatus::Running => "running",
        AgentStatus::Blocked => "blocked",
        AgentStatus::Done => "done",
        AgentStatus::Error => "error",
    }
}

pub fn summarize_lean_error(result: &LeanVerificationSummary) -> String {
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

pub fn default_session_with_workspace(
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
