use chrono::Utc;
use openproof_protocol::{
    AgentRecord, AgentRole, AgentStatus, AgentTask, BranchMessage, BranchQueueState, MessageRole,
    ProofBranch, ProofNode, ProofNodeKind, ProofNodeStatus, TranscriptEntry,
};

use crate::helpers::{agent_role_label, next_id, phase_from_role};
use crate::state::{AppState, AutonomousRunPatch, PendingWrite};

impl AppState {
    pub fn add_proof_node(
        &mut self,
        kind: ProofNodeKind,
        label: &str,
        statement: &str,
    ) -> Result<PendingWrite, String> {
        let label = label.trim();
        let statement = statement.trim();
        if label.is_empty() || statement.is_empty() {
            return Err(
                "Usage: /theorem <label> :: <statement> or /lemma <label> :: <statement>"
                    .to_string(),
            );
        }
        let timestamp = Utc::now().to_rfc3339();
        let snapshot = {
            let Some(session) = self.current_session_mut() else {
                return Err("No active session.".to_string());
            };
            // If adding a sub-lemma, set parent to the current active node
            let parent = session.proof.active_node_id.clone();
            let depth = if kind == ProofNodeKind::Lemma {
                parent
                    .as_deref()
                    .and_then(|pid| session.proof.nodes.iter().find(|n| n.id == pid))
                    .map(|p| p.depth + 1)
                    .unwrap_or(0)
            } else {
                0
            };
            let node = ProofNode {
                id: next_id("node"),
                kind,
                label: label.to_string(),
                statement: statement.to_string(),
                content: String::new(),
                status: ProofNodeStatus::Pending,
                parent_id: if depth > 0 { parent } else { None },
                depends_on: Vec::new(),
                depth,
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

    pub fn set_active_node(
        &mut self,
        node_id: Option<&str>,
    ) -> Result<Option<PendingWrite>, String> {
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
            let promoted_goal_summary = branch.latest_goals.clone().or_else(|| {
                (!branch.goal_summary.trim().is_empty()).then(|| branch.goal_summary.clone())
            });
            let promoted_latest_diagnostics = if resolved {
                None
            } else {
                branch.latest_diagnostics.clone().or_else(|| {
                    (!branch.last_lean_diagnostic.trim().is_empty())
                        .then(|| branch.last_lean_diagnostic.clone())
                })
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
                search_history: vec![],
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
            session.proof.hidden_branch_count = session
                .proof
                .branches
                .iter()
                .filter(|branch| branch.hidden)
                .count()
                + if hidden { 1 } else { 0 };
            session.proof.phase = phase_from_role(role).to_string();
            session.proof.status_line =
                format!("{} branch running: {}", agent_role_label(role), title);
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
}
