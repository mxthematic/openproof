use chrono::Utc;
use openproof_protocol::{
    CloudPolicy, LeanVerificationSummary, MessageRole, ProofNode, ProofNodeKind, ProofNodeStatus,
    ProofQuestionOption, ProofQuestionState, ProofSessionState, ShareMode, TranscriptEntry,
};
use serde_json::Value;

pub(crate) fn default_proof_state() -> ProofSessionState {
    ProofSessionState {
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
        full_autonomous: false,
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
        workspace_files: Vec::new(),
        tool_iteration_count: 0,
        search_strategy: Default::default(),
    }
}

pub(crate) fn extract_cloud_policy(value: &Value) -> CloudPolicy {
    let share_mode = match value
        .get("cloud")
        .and_then(|item| item.get("shareMode"))
        .and_then(Value::as_str)
        .unwrap_or("local")
    {
        "community" => ShareMode::Community,
        "private" => ShareMode::Private,
        _ => ShareMode::Local,
    };
    CloudPolicy {
        sync_enabled: value
            .get("cloud")
            .and_then(|item| item.get("syncEnabled"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        share_mode,
        private_overlay_community: value
            .get("cloud")
            .and_then(|item| item.get("privateOverlayCommunity"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        last_sync_at: value
            .get("cloud")
            .and_then(|item| item.get("lastSyncAt"))
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

pub(crate) fn extract_proof_state(value: &Value) -> ProofSessionState {
    let mut proof = default_proof_state();
    proof.problem = value
        .get("proof")
        .and_then(|item| item.get("intent"))
        .and_then(|item| item.get("problem"))
        .and_then(Value::as_str)
        .map(str::to_string);
    proof.formal_target = value
        .get("proof")
        .and_then(|item| item.get("intent"))
        .and_then(|item| item.get("formalTarget"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            value
                .get("proof")
                .and_then(|item| item.get("formalization"))
                .and_then(|item| item.get("provisionalTarget"))
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    proof.accepted_target = value
        .get("proof")
        .and_then(|item| item.get("intent"))
        .and_then(|item| item.get("acceptedTarget"))
        .and_then(Value::as_str)
        .map(str::to_string);
    proof.search_status = value
        .get("proof")
        .and_then(|item| item.get("lastSearchStatus"))
        .and_then(Value::as_str)
        .map(str::to_string);
    proof.assumptions = value
        .get("proof")
        .and_then(|item| item.get("formalization"))
        .and_then(|item| item.get("assumptions"))
        .and_then(extract_string_array)
        .unwrap_or_default();
    proof.paper_notes = value
        .get("paper")
        .and_then(|item| item.get("notes"))
        .and_then(Value::as_array)
        .map(|notes| {
            notes
                .iter()
                .filter_map(|note| note.get("text").and_then(Value::as_str).map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    proof.pending_question = extract_pending_question(value);
    proof.awaiting_clarification = value
        .get("proof")
        .and_then(|item| item.get("awaitingClarification"))
        .and_then(Value::as_bool)
        .unwrap_or(proof.pending_question.is_some());
    proof.is_autonomous_running = value
        .get("proof")
        .and_then(|item| item.get("isAutonomousRunning"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    proof.autonomous_iteration_count = value
        .get("proof")
        .and_then(|item| item.get("autonomousIterationCount"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    proof.autonomous_started_at = value
        .get("proof")
        .and_then(|item| item.get("autonomousStartedAt"))
        .and_then(Value::as_str)
        .map(str::to_string);
    proof.autonomous_last_progress_at = value
        .get("proof")
        .and_then(|item| item.get("autonomousLastProgressAt"))
        .and_then(Value::as_str)
        .map(str::to_string);
    proof.autonomous_pause_reason = value
        .get("proof")
        .and_then(|item| item.get("autonomousPauseReason"))
        .and_then(Value::as_str)
        .map(str::to_string);
    proof.autonomous_stop_reason = value
        .get("proof")
        .and_then(|item| item.get("autonomousStopReason"))
        .and_then(Value::as_str)
        .map(str::to_string);
    proof.hidden_best_branch_id = value
        .get("proof")
        .and_then(|item| item.get("hiddenBestBranchId"))
        .and_then(Value::as_str)
        .map(str::to_string);
    proof.active_retrieval_summary = value
        .get("proof")
        .and_then(|item| item.get("activeRetrievalSummary"))
        .and_then(Value::as_str)
        .map(str::to_string);
    proof.strategy_summary = value
        .get("proof")
        .and_then(|item| item.get("strategySummary"))
        .and_then(Value::as_str)
        .map(str::to_string);
    proof.goal_summary = value
        .get("proof")
        .and_then(|item| item.get("goalSummary"))
        .and_then(Value::as_str)
        .map(str::to_string);
    proof.latest_diagnostics = value
        .get("proof")
        .and_then(|item| item.get("latestDiagnostics"))
        .and_then(Value::as_str)
        .map(str::to_string);
    proof.phase = value
        .get("proof")
        .and_then(|item| item.get("phase"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| proof.phase.clone());
    proof.status_line = value
        .get("proof")
        .and_then(|item| item.get("statusLine"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| proof.status_line.clone());
    proof.active_node_id = value
        .get("activeNodeId")
        .and_then(Value::as_str)
        .map(str::to_string);
    proof.root_node_id = value
        .get("proof")
        .and_then(|item| item.get("rootNodeId"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| proof.active_node_id.clone());
    proof.nodes = extract_proof_nodes(value);
    proof.last_verification = extract_last_verification(value);
    if let Some(result) = &proof.last_verification {
        proof.last_rendered_scratch = Some(result.rendered_scratch.clone());
    }
    proof
}

fn extract_string_array(value: &Value) -> Option<Vec<String>> {
    value.as_array().map(|items| {
        items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>()
    })
}

fn extract_pending_question(value: &Value) -> Option<ProofQuestionState> {
    let raw = value.get("proof").and_then(|item| item.get("pendingQuestion"))?;
    let prompt = raw.get("prompt").and_then(Value::as_str)?.trim().to_string();
    if prompt.is_empty() {
        return None;
    }
    let options = raw
        .get("options")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let id = item.get("id").and_then(Value::as_str)?.trim().to_string();
                    if id.is_empty() {
                        return None;
                    }
                    Some(ProofQuestionOption {
                        id: id.clone(),
                        label: item
                            .get("label")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or(id),
                        summary: item
                            .get("summary")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_default(),
                        formal_target: item
                            .get("formalTarget")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_default(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Some(ProofQuestionState {
        prompt,
        options,
        recommended_option_id: raw
            .get("recommendedOptionId")
            .and_then(Value::as_str)
            .map(str::to_string),
        answer_text: raw
            .get("answerText")
            .and_then(Value::as_str)
            .map(str::to_string),
        status: raw
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| "open".to_string()),
    })
}

fn extract_proof_nodes(value: &Value) -> Vec<ProofNode> {
    let Some(nodes) = value.get("nodes").and_then(Value::as_array) else {
        return Vec::new();
    };
    nodes
        .iter()
        .filter_map(|node| {
            let kind = match node.get("kind").and_then(Value::as_str).unwrap_or("theorem") {
                "lemma" => ProofNodeKind::Lemma,
                "theorem" => ProofNodeKind::Theorem,
                "artifact" => ProofNodeKind::Artifact,
                "attempt" => ProofNodeKind::Attempt,
                "conjecture" => ProofNodeKind::Conjecture,
                _ => return None,
            };
            let label = node.get("label").and_then(Value::as_str)?.trim().to_string();
            let statement = node
                .get("statement")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if label.is_empty() || statement.is_empty() {
                return None;
            }
            let status = match node
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("pending")
            {
                "suggested" => ProofNodeStatus::Suggested,
                "proving" => ProofNodeStatus::Proving,
                "verifying" => ProofNodeStatus::Verifying,
                "verified" => ProofNodeStatus::Verified,
                "failed" => ProofNodeStatus::Failed,
                "abandoned" => ProofNodeStatus::Abandoned,
                _ => ProofNodeStatus::Pending,
            };
            Some(ProofNode {
                id: node
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("legacy_node_{}", label)),
                kind,
                label,
                statement,
                content: node
                    .get("content")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_default(),
                status,
                created_at: node
                    .get("createdAt")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| Utc::now().to_rfc3339()),
                parent_id: node
                    .get("parentId")
                    .or_else(|| node.get("parent_id"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
                depends_on: node
                    .get("dependsOn")
                    .or_else(|| node.get("depends_on"))
                    .and_then(Value::as_array)
                    .map(|arr| arr.iter().filter_map(Value::as_str).map(str::to_string).collect())
                    .unwrap_or_default(),
                depth: node
                    .get("depth")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize,
                updated_at: node
                    .get("updatedAt")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| Utc::now().to_rfc3339()),
            })
        })
        .collect()
}

pub(crate) fn extract_last_verification(value: &Value) -> Option<LeanVerificationSummary> {
    let raw = value.get("runtime").and_then(|item| item.get("lastLeanCheck"))?;
    Some(LeanVerificationSummary {
        ok: raw.get("ok").and_then(Value::as_bool).unwrap_or(false),
        code: raw
            .get("code")
            .and_then(Value::as_i64)
            .map(|value| value as i32),
        stdout: raw
            .get("stdout")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_default(),
        stderr: raw
            .get("stderr")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_default(),
        error: raw.get("error").and_then(Value::as_str).map(str::to_string),
        checked_at: raw
            .get("checkedAt")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| Utc::now().to_rfc3339()),
        project_dir: raw
            .get("projectDir")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_default(),
        scratch_path: raw
            .get("scratchPath")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_default(),
        rendered_scratch: raw
            .get("renderedScratch")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_default(),
    })
}

pub(crate) fn extract_transcript(value: &Value) -> Vec<TranscriptEntry> {
    let mut transcript = Vec::new();

    if let Some(messages) = value.get("messages").and_then(Value::as_array) {
        for message in messages {
            let role = match message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("notice")
            {
                "user" => MessageRole::User,
                "assistant" => MessageRole::Assistant,
                "system" => MessageRole::System,
                _ => MessageRole::Notice,
            };
            transcript.push(TranscriptEntry {
                id: message
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("msg_{}", transcript.len())),
                role,
                title: None,
                content: message
                    .get("content")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_default(),
                created_at: message
                    .get("createdAt")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| Utc::now().to_rfc3339()),
            });
        }
    }

    if let Some(events) = value.get("events").and_then(Value::as_array) {
        for event in events {
            let title = event
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_string);
            let detail = event
                .get("detail")
                .and_then(Value::as_str)
                .unwrap_or_default();
            transcript.push(TranscriptEntry {
                id: event
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("evt_{}", transcript.len())),
                role: MessageRole::Notice,
                title,
                content: detail.to_string(),
                created_at: event
                    .get("createdAt")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| Utc::now().to_rfc3339()),
            });
        }
    }

    transcript.sort_by(|left, right| left.created_at.cmp(&right.created_at));
    transcript
}
