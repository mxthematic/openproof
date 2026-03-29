use chrono::Utc;
use openproof_protocol::{
    AgentStatus, BranchMessage, BranchQueueState, MessageRole, ProofNodeStatus, TranscriptEntry,
};

use crate::helpers::{agent_role_label, next_id, phase_from_role};
use crate::parser::{derive_goal_label, extract_lean_code_block, parse_assistant_output};
use crate::state::{AppState, PendingWrite};

/// Derive nogood context from the proof tree: find failed decompositions
/// where a parent node's children have failed, and format as warnings
/// for the Planner to avoid repeating those patterns.
pub fn derive_nogood_context(nodes: &[openproof_protocol::ProofNode]) -> String {
    let mut nogoods = Vec::new();

    for node in nodes {
        if node.parent_id.is_none() {
            continue;
        }
        // Find children of this node that have failed.
        let children: Vec<&openproof_protocol::ProofNode> = nodes
            .iter()
            .filter(|n| n.parent_id.as_deref() == Some(&node.id))
            .collect();
        if children.is_empty() {
            continue;
        }
        let failed_children: Vec<&openproof_protocol::ProofNode> = children
            .iter()
            .filter(|c| {
                c.status == ProofNodeStatus::Failed || c.status == ProofNodeStatus::Abandoned
            })
            .copied()
            .collect();
        if failed_children.is_empty() {
            continue;
        }
        // This node had a decomposition where children failed.
        let sub_types: Vec<String> = children.iter().map(|c| c.statement.clone()).collect();
        let failed_labels: Vec<String> = failed_children.iter().map(|c| c.label.clone()).collect();
        nogoods.push(format!(
            "- Decomposed '{}' into [{}]. Failed on: [{}]",
            node.label,
            sub_types.join(", "),
            failed_labels.join(", "),
        ));
    }

    if nogoods.is_empty() {
        return String::new();
    }

    format!(
        "\n\nPREVIOUS FAILED DECOMPOSITIONS (do NOT repeat):\n{}\n",
        nogoods.join("\n"),
    )
}

impl AppState {
    pub(crate) fn apply_append_assistant(&mut self, content: String) -> Option<PendingWrite> {
        let text = content.trim();
        if text.is_empty() {
            return None;
        }
        let parsed = parse_assistant_output(text);
        let tools_active = self.tool_loop_active;
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
            if let Some(problem) = parsed
                .problem
                .as_ref()
                .filter(|item| !item.trim().is_empty())
            {
                session.proof.problem = Some(problem.trim().to_string());
            }
            if let Some(formal_target) = parsed
                .formal_target
                .as_ref()
                .filter(|item| !item.trim().is_empty())
            {
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
            if let Some(search_status) = parsed
                .search_status
                .as_ref()
                .filter(|item| !item.trim().is_empty())
            {
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
            // Consistency check: reject obviously bad decompositions.
            let parent_statement = session
                .proof
                .active_node_id
                .as_deref()
                .and_then(|pid| session.proof.nodes.iter().find(|n| n.id == pid))
                .map(|n| n.statement.as_str())
                .unwrap_or("");
            let sub_lemmas: Vec<(String, String)> = parsed
                .created_nodes
                .iter()
                .map(|c| (c.label.clone(), c.statement.clone()))
                .collect();
            if !sub_lemmas.is_empty() {
                let issues = crate::decomposition_checks::check_decomposition_consistency(
                    parent_statement,
                    &sub_lemmas,
                );
                if !issues.is_empty() {
                    session.transcript.push(TranscriptEntry {
                        id: next_id("native_msg"),
                        role: MessageRole::Notice,
                        title: Some("Decomposition Warning".to_string()),
                        content: format!("Issues with proposed sub-lemmas: {}", issues.join("; ")),
                        created_at: session.updated_at.clone(),
                    });
                }
            }
            for created in &parsed.created_nodes {
                // Sub-lemmas are children of the current active node
                let parent = session.proof.active_node_id.clone();
                let depth = parent
                    .as_deref()
                    .and_then(|pid| session.proof.nodes.iter().find(|n| n.id == pid))
                    .map(|p| p.depth + 1)
                    .unwrap_or(0);
                let node = openproof_protocol::ProofNode {
                    id: next_id("node"),
                    kind: created.kind,
                    label: created.label.clone(),
                    statement: created.statement.clone(),
                    content: String::new(),
                    status: ProofNodeStatus::Pending,
                    parent_id: if depth > 0 { parent.clone() } else { None },
                    depends_on: Vec::new(),
                    depth,
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
                        parsed
                            .title
                            .as_deref()
                            .or(Some(session.title.as_str()))
                            .unwrap_or("Goal"),
                    );
                    let node = openproof_protocol::ProofNode {
                        id: next_id("node"),
                        kind: openproof_protocol::ProofNodeKind::Theorem,
                        label,
                        statement: target,
                        content: String::new(),
                        status: ProofNodeStatus::Pending,
                        parent_id: None,
                        depends_on: Vec::new(),
                        depth: 0,
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
            // When tools are active, workspace files are the source of truth.
            // Do NOT extract lean code blocks from text -- that destroys working code.
            let mut lean_applied = false;
            if !tools_active {
                if let Some(candidate) = parsed
                    .lean_snippets
                    .first()
                    .cloned()
                    .or_else(|| extract_lean_code_block(text))
                {
                    lean_applied = true;
                    let active_node_id = session.proof.active_node_id.clone();
                    if let Some(node_id) = active_node_id {
                        if let Some(node) = session
                            .proof
                            .nodes
                            .iter_mut()
                            .find(|node| node.id == node_id)
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
                }
            }
            if !lean_applied {
                if let Some(next_step) = parsed
                    .next_steps
                    .first()
                    .filter(|item| !item.trim().is_empty())
                {
                    session.proof.status_line = next_step.trim().to_string();
                }
            }
            (session.clone(), session.proof.status_line.clone())
        }) {
            self.sync_question_selection();
            self.pending_writes += 1;
            self.status = status_line;
            return Some(PendingWrite { session: snapshot });
        }
        None
    }

    pub(crate) fn apply_append_branch_assistant(
        &mut self,
        branch_id: String,
        content: String,
        used_tools: bool,
    ) -> Option<PendingWrite> {
        let text = content.trim();
        if text.is_empty() {
            return None;
        }
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
                if let Some(search_status) = parsed
                    .search_status
                    .as_ref()
                    .filter(|item| !item.trim().is_empty())
                {
                    branch.search_status = search_status.trim().to_string();
                }
                if let Some(phase) = parsed.phase.as_ref().filter(|item| !item.trim().is_empty()) {
                    branch.phase = Some(phase.trim().to_string());
                    branch.progress_kind = Some(phase.trim().to_string());
                }
                // When tools were used, workspace files are the source of truth.
                // Do NOT extract lean code from text -- that destroys working code
                // written by file_write/file_patch tools.
                if !used_tools {
                    // Check for patch format first -- surgical edits preferred over full rewrites
                    let snippet_from_patch = if openproof_lean::patch::contains_patch(text) {
                        if let Some(patch_text) = openproof_lean::patch::extract_patch(text) {
                            let current = session.proof.last_rendered_scratch
                                .as_deref()
                                .or_else(|| session.proof.nodes.iter()
                                    .find(|n| Some(n.id.as_str()) == session.proof.active_node_id.as_deref())
                                    .map(|n| n.content.as_str()))
                                .unwrap_or("");
                            openproof_lean::patch::apply_patch(current, &patch_text)
                                .map(|result| {
                                    // Add patch diff as a visible notice
                                    session.transcript.push(TranscriptEntry {
                                        id: next_id("native_msg"),
                                        role: MessageRole::Notice,
                                        title: Some("Patch".to_string()),
                                        content: format!(
                                            "Applied patch: {}\n{}",
                                            result.diff_summary.lines().next().unwrap_or(""),
                                            result.diff_summary.lines().skip(1).take(6).collect::<Vec<_>>().join("\n"),
                                        ),
                                        created_at: now.clone(),
                                    });
                                    result.patched_content
                                })
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    if let Some(snippet) = snippet_from_patch
                        .or_else(|| parsed.lean_snippets.first().cloned())
                        .or_else(|| extract_lean_code_block(text))
                    {
                        branch.lean_snippet = snippet.clone();
                        branch.search_status = "candidate updated".to_string();

                        // Visible notice: agent found a lean candidate
                        let snippet_lines = snippet.lines().count();
                        let role_label = agent_role_label(branch.role);
                        session.transcript.push(TranscriptEntry {
                            id: next_id("native_msg"),
                            role: MessageRole::Notice,
                            title: Some("Lean".to_string()),
                            content: format!("{role_label} produced Lean candidate ({snippet_lines} lines). Verifying..."),
                            created_at: now.clone(),
                        });
                        branch.progress_kind = Some(
                            if branch.role == openproof_protocol::AgentRole::Repairer {
                                "repairing".to_string()
                            } else {
                                "candidate".to_string()
                            },
                        );
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
                }
                if !parsed.next_steps.is_empty() {
                    branch.summary = parsed.next_steps.join(" ");
                } else if !parsed.paper_notes.is_empty() {
                    branch.summary = parsed.paper_notes.join(" ");
                }
                if branch.role == openproof_protocol::AgentRole::Planner
                    && !branch.summary.trim().is_empty()
                {
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
                        .or_else(|| {
                            (!branch.goal_summary.trim().is_empty())
                                .then(|| branch.goal_summary.clone())
                        });
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
        None
    }

    pub(crate) fn apply_finish_branch(
        &mut self,
        branch_id: String,
        status: AgentStatus,
        summary: String,
        output: String,
    ) -> Option<PendingWrite> {
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
                    content: format!(
                        "Branch {}: {}",
                        crate::helpers::format_agent_status(status),
                        summary
                    ),
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
            // Add a visible notice to the main transcript so the user sees branch activity
            let role_label = crate::helpers::agent_role_label(
                session
                    .proof
                    .branches
                    .iter()
                    .find(|b| b.id == branch_id)
                    .map(|b| b.role)
                    .unwrap_or(openproof_protocol::AgentRole::Prover),
            );
            let notice_content = match status {
                AgentStatus::Done => format!("{role_label}: {summary}"),
                AgentStatus::Error => format!("{role_label} error: {summary}"),
                AgentStatus::Blocked => format!("{role_label} blocked: {summary}"),
                _ => format!("{role_label}: {summary}"),
            };
            session.transcript.push(TranscriptEntry {
                id: next_id("native_msg"),
                role: MessageRole::Notice,
                title: Some("Agent".to_string()),
                content: notice_content,
                created_at: now,
            });

            session.proof.status_line = summary.clone();
            (session.clone(), summary)
        }) {
            self.pending_writes += 1;
            self.status = status_line;
            return Some(PendingWrite { session: snapshot });
        }
        None
    }

    pub(crate) fn apply_append_notice(
        &mut self,
        title: String,
        content: String,
    ) -> Option<PendingWrite> {
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
        None
    }

    pub(crate) fn apply_sync_completed(&mut self) -> Option<PendingWrite> {
        if let Ok(write) = self.mark_sync_completed() {
            return Some(write);
        }
        None
    }

    pub(crate) fn apply_tool_call_received(
        &mut self,
        _call_id: String,
        tool_name: String,
        arguments: String,
    ) -> Option<PendingWrite> {
        self.tool_loop_active = true;
        self.current_tool_name = Some(tool_name.clone());
        // Set human-readable activity label based on tool name
        self.activity_label = match tool_name.as_str() {
            "lean_verify" => "verifying proof...".to_string(),
            "lean_goals" => "extracting goals...".to_string(),
            "lean_screen_tactics" => "testing tactics...".to_string(),
            "lean_check" => "checking types...".to_string(),
            "lean_search_tactic" => "searching for tactic...".to_string(),
            "corpus_search" => "searching corpus...".to_string(),
            "file_write" => "writing file...".to_string(),
            "file_patch" => "patching file...".to_string(),
            "file_read" => "reading file...".to_string(),
            "workspace_ls" => "listing workspace...".to_string(),
            "shell_run" => "running command...".to_string(),
            _ => format!("running {tool_name}..."),
        };
        self.activity_started_at = Some(std::time::Instant::now());
        // Push to session activity log
        let activity_msg = self.activity_label.clone();
        if let Some(session) = self.current_session_mut() {
            session
                .proof
                .activity_log
                .push(openproof_protocol::ActivityEntry {
                    timestamp: Utc::now().to_rfc3339(),
                    kind: "tool".to_string(),
                    message: activity_msg,
                });
            // Cap at 50 entries
            if session.proof.activity_log.len() > 50 {
                session
                    .proof
                    .activity_log
                    .drain(..session.proof.activity_log.len() - 50);
            }
        }
        let entry = TranscriptEntry {
            id: next_id("tool_call"),
            role: MessageRole::ToolCall,
            title: Some(tool_name),
            content: arguments,
            created_at: Utc::now().to_rfc3339(),
        };
        if let Some(snapshot) = self.current_session_mut().map(|session| {
            session.updated_at = entry.created_at.clone();
            session.transcript.push(entry);
            session.clone()
        }) {
            self.pending_writes += 1;
            return Some(PendingWrite { session: snapshot });
        }
        None
    }

    pub(crate) fn apply_tool_result_received(
        &mut self,
        _call_id: String,
        tool_name: String,
        success: bool,
        output: String,
    ) -> Option<PendingWrite> {
        self.current_tool_name = None;
        self.activity_label = "thinking...".to_string();
        self.activity_started_at = Some(std::time::Instant::now());
        let status_word = if success { "ok" } else { "failed" };
        self.status = format!("Tool {tool_name}: {status_word}");
        let entry = TranscriptEntry {
            id: next_id("tool_result"),
            role: MessageRole::ToolResult,
            title: Some(tool_name),
            content: output,
            created_at: Utc::now().to_rfc3339(),
        };
        if let Some(snapshot) = self.current_session_mut().map(|session| {
            session.updated_at = entry.created_at.clone();
            session.transcript.push(entry);
            session.clone()
        }) {
            self.pending_writes += 1;
            return Some(PendingWrite { session: snapshot });
        }
        None
    }

    /// Sync workspace file content into the active proof node.
    /// Called after tool-using turns so node.content reflects what
    /// file_write/file_patch tools actually wrote to the workspace.
    pub(crate) fn apply_workspace_content_sync(
        &mut self,
        content: String,
        verified: bool,
    ) -> Option<PendingWrite> {
        if content.trim().is_empty() {
            return None;
        }
        if let Some(snapshot) = self.current_session_mut().map(|session| {
            let now = Utc::now().to_rfc3339();
            session.updated_at = now.clone();
            session.proof.last_rendered_scratch = Some(content.clone());
            let active_node_id = session.proof.active_node_id.clone();
            if let Some(node_id) = active_node_id {
                if let Some(node) = session
                    .proof
                    .nodes
                    .iter_mut()
                    .find(|node| node.id == node_id)
                {
                    // Never mark as verified if content has sorry or is vacuous
                    let is_vacuous = content.contains(": True :=")
                        || content.contains(": True by")
                        || content.lines().any(|l| {
                            let t = l.trim();
                            t.starts_with("axiom ") || t.starts_with("constant ")
                        });
                    let actually_verified = verified && !content.contains("sorry") && !is_vacuous;
                    node.content = content;
                    node.status = if actually_verified {
                        ProofNodeStatus::Verified
                    } else {
                        ProofNodeStatus::Proving
                    };
                    node.updated_at = now.clone();
                    if verified {
                        session.proof.phase = "done".to_string();
                        session.proof.status_line =
                            format!("Lean verified {} via tool loop.", node.label);
                    } else {
                        session.proof.phase = "proving".to_string();
                        session.proof.status_line =
                            format!("Synced workspace code for {}.", node.label);
                    }
                }
            }
            session.clone()
        }) {
            self.pending_writes += 1;
            return Some(PendingWrite { session: snapshot });
        }
        None
    }
}
