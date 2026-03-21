use crate::helpers::{agent_role_label, format_agent_status, format_node_status};
use crate::state::AppState;
use openproof_protocol::{AgentTask, ProofNodeStatus};

impl AppState {
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
                    .map(|task: &AgentTask| format!(" · {}", task.title))
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
}
