use chrono::Utc;
use openproof_protocol::{
    AgentStatus, BranchQueueState, MessageRole, ProofNodeStatus, TranscriptEntry,
};

use crate::helpers::{agent_role_label, next_id, summarize_lean_error};
use crate::state::{AppState, AppEvent, PendingWrite};

impl AppState {
    pub(crate) fn apply_turn_started(&mut self) {
        self.turn_in_flight = true;
        self.turn_started_at = Some(std::time::Instant::now());
        self.streaming_text.clear();
        self.status = "Running assistant turn...".to_string();
    }

    pub(crate) fn apply_reasoning_started(&mut self) {
        self.status = "Reasoning...".to_string();
    }

    pub(crate) fn apply_stream_delta(&mut self, delta: String) {
        self.streaming_text.push_str(&delta);
        if self.status.starts_with("Reasoning") {
            self.status = "Streaming response...".to_string();
        }
    }

    pub(crate) fn apply_stream_finished(&mut self) -> Option<PendingWrite> {
        let text = std::mem::take(&mut self.streaming_text);
        if !text.trim().is_empty() {
            return self.apply(AppEvent::AppendAssistant(text));
        }
        None
    }

    pub(crate) fn apply_turn_finished(&mut self) {
        // Flush any remaining streaming text
        if !self.streaming_text.is_empty() {
            let text = std::mem::take(&mut self.streaming_text);
            if !text.trim().is_empty() {
                let _ = self.apply(AppEvent::AppendAssistant(text));
            }
        }
        self.turn_in_flight = false;
        self.turn_started_at = None;
        self.tool_loop_active = false;
        self.tool_loop_iteration = 0;
        self.current_tool_name = None;
        if !self.verification_in_flight {
            self.status = "Ready.".to_string();
        }
    }

    pub(crate) fn apply_lean_verify_started(&mut self) {
        self.verification_in_flight = true;
        self.status = "Verifying with Lean...".to_string();
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

    pub(crate) fn apply_lean_verify_finished(
        &mut self,
        result: openproof_protocol::LeanVerificationSummary,
    ) -> Option<PendingWrite> {
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

            // Parse the Lean file to extract proof tree structure.
            // Only replace the full node list if the parsed result has MORE
            // declarations than the current list. Otherwise the rendered_scratch
            // might only contain one node (the active one), and replacing would
            // wipe multi-theorem sessions.
            let parsed_decls = openproof_lean::parse_lean_declarations(&result.rendered_scratch);
            if !parsed_decls.is_empty() && parsed_decls.len() >= session.proof.nodes.len() {
                let parsed_nodes = openproof_lean::declarations_to_proof_nodes(
                    &parsed_decls,
                    &session.id,
                );
                let old_statuses: std::collections::HashMap<String, ProofNodeStatus> = session
                    .proof.nodes.iter()
                    .map(|n| (n.label.clone(), n.status))
                    .collect();
                let active_label = session.proof.active_node_id.as_deref()
                    .and_then(|id| session.proof.nodes.iter().find(|n| n.id == id))
                    .map(|n| n.label.clone());

                session.proof.nodes = parsed_nodes.iter().map(|pn| {
                    let mut node = pn.clone();
                    if result.ok {
                        if let Some(&prev_status) = old_statuses.get(&node.label) {
                            if prev_status != ProofNodeStatus::Pending {
                                node.status = prev_status;
                            }
                        }
                    }
                    node.updated_at = now.clone();
                    node
                }).collect();

                if let Some(label) = &active_label {
                    session.proof.active_node_id = session.proof.nodes.iter()
                        .find(|n| &n.label == label)
                        .map(|n| n.id.clone());
                }
                if let Some(root) = session.proof.nodes.first() {
                    session.proof.root_node_id = Some(root.id.clone());
                }
            }

            // Mark ALL nodes individually based on per-declaration sorry analysis.
            // When the file compiles, each node is verified/failed based on its own content.
            // When the file doesn't compile, all nodes are failed.
            for node in session.proof.nodes.iter_mut() {
                let node_has_sorry = node.content.contains("sorry");
                let is_vacuous = node.content.contains(": True :=")
                    || node.content.contains(": True by")
                    || node.content.lines().any(|l| {
                        let t = l.trim();
                        t.starts_with("axiom ") || t.starts_with("constant ")
                    });
                if result.ok && !node_has_sorry && !is_vacuous {
                    node.status = ProofNodeStatus::Verified;
                } else if !result.ok {
                    node.status = ProofNodeStatus::Failed;
                } else {
                    // File compiled but this node has sorry
                    node.status = ProofNodeStatus::Failed;
                }
                node.updated_at = now.clone();
            }

            // Phase from aggregate status
            let all_verified = session.proof.nodes.iter()
                .all(|n| n.status == ProofNodeStatus::Verified);
            let verified_count = session.proof.nodes.iter()
                .filter(|n| n.status == ProofNodeStatus::Verified).count();
            let total = session.proof.nodes.len();
            session.proof.phase = if all_verified && total > 0 {
                "done".to_string()
            } else {
                "repairing".to_string()
            };
            session.proof.status_line = if all_verified && total > 0 {
                "All theorems verified.".to_string()
            } else if result.ok {
                format!("{verified_count}/{total} nodes verified (sorry in remaining).")
            } else {
                "Lean compilation failed.".to_string()
            };
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
        None
    }

    pub(crate) fn apply_branch_verify_finished(
        &mut self,
        branch_id: String,
        focus_node_id: Option<String>,
        promote: bool,
        result: openproof_protocol::LeanVerificationSummary,
    ) -> Option<PendingWrite> {
        self.verification_in_flight = false;
        let now = Utc::now().to_rfc3339();
        if let Some(snapshot) = self.current_session_mut().map(|session| {
            // Parse Lean declarations from the rendered scratch to build proof tree
            if !result.rendered_scratch.is_empty() {
                let parsed_decls = openproof_lean::parse_lean_declarations(&result.rendered_scratch);
                if !parsed_decls.is_empty() {
                    let parsed_nodes = openproof_lean::declarations_to_proof_nodes(
                        &parsed_decls,
                        &session.id,
                    );
                    for pn in &parsed_nodes {
                        if let Some(existing) = session.proof.nodes.iter_mut().find(|n| n.label == pn.label) {
                            existing.content = pn.content.clone();
                            existing.statement = pn.statement.clone();
                            existing.kind = pn.kind;
                            existing.parent_id = pn.parent_id.clone();
                            existing.depth = pn.depth;
                            existing.updated_at = now.clone();
                        } else {
                            let mut new_node = pn.clone();
                            new_node.status = openproof_protocol::ProofNodeStatus::Pending;
                            new_node.updated_at = now.clone();
                            session.proof.nodes.push(new_node);
                        }
                    }
                }
            }

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
                if let Some(node) = session
                    .proof
                    .nodes
                    .iter_mut()
                    .find(|node| node.id == node_id)
                {
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
                .and_then(|node_id| {
                    session
                        .proof
                        .nodes
                        .iter()
                        .find(|node| node.id == node_id)
                })
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
                content: if result.ok {
                    status_line.clone()
                } else {
                    diagnostic.clone()
                },
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
        None
    }
}
