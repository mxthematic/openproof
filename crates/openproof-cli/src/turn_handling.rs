//! Turn lifecycle: submitting prompts, spawning agent branches, and
//! launching branch Lean verification.
//!
//! A "turn" is one round-trip to the model (either the main session transcript
//! or a named agent branch).  This module owns the async spawning logic that
//! feeds `AppEvent`s back to the event loop.

use crate::helpers::{
    agent_role_label, branch_phase_for_role, emit_local_notice, persist_current_session,
    persist_write, resolve_lean_project_dir, summarize_branch_output, truncate,
};
use crate::system_prompt::{build_branch_turn_messages, build_turn_messages_with_retrieval};
use openproof_core::{AppEvent, AppState, PendingWrite, SubmittedInput};
use openproof_lean::lsp_mcp::LeanLspMcp;
use openproof_lean::tools::{execute_tool, ToolContext, ToolOutput};
use std::sync::{Arc, Mutex};
use openproof_model::{run_codex_turn, 
    run_codex_turn_with_events, CodexTurnRequest, StreamEvent, TurnMessage,
};
use openproof_protocol::{AgentRole, AgentStatus, BranchQueueState, SessionSnapshot};
use openproof_store::AppStore;
use tokio::sync::mpsc;

/// Maximum number of tool-loop iterations per turn.
const MAX_TOOL_ITERATIONS: usize = 25;

pub fn handle_submission(
    tx: mpsc::UnboundedSender<AppEvent>,
    store: AppStore,
    state: &mut AppState,
    submission: SubmittedInput,
) {
    if submission.raw_text.trim_start().starts_with('/') {
        crate::slash_commands::apply_local_command(tx, state, store, submission);
        return;
    }

    if state.turn_in_flight {
        let _ = tx.send(AppEvent::AppendNotice {
            title: "Busy".to_string(),
            content: "A model turn is already running. Wait for it to finish before submitting another prompt.".to_string(),
        });
        return;
    }

    let _ = state.apply(AppEvent::TurnStarted);
    let session_snapshot = submission.session_snapshot.clone();
    let tx_model = tx.clone();
    let store_for_model = store.clone();
    tokio::spawn(async move {
        let messages =
            build_turn_messages_with_retrieval(&store_for_model, Some(&session_snapshot)).await;

        run_agentic_loop(
            tx_model,
            store_for_model,
            &submission.session_id,
            messages,
            &session_snapshot,
        )
        .await;
    });
}

/// Run the agentic loop: call the model, execute tool calls, repeat.
/// Public so headless runner can use it too.
pub async fn run_agentic_loop(
    tx: mpsc::UnboundedSender<AppEvent>,
    store: AppStore,
    session_id: &str,
    initial_messages: Vec<TurnMessage>,
    session: &SessionSnapshot,
) {
    let mut messages = initial_messages;
    let project_dir = resolve_lean_project_dir();
    let workspace_dir = store.workspace_dir(session_id);
    // Ensure workspace directory exists.
    let _ = std::fs::create_dir_all(&workspace_dir);
    let mut turn_used_tools = false;
    let mut last_verify_ok = false;

    // Spawn lean-lsp-mcp for structured goal access (tools: lean_goals, lean_screen_tactics).
    // Falls back gracefully if unavailable.
    let lsp_mcp: Option<Arc<Mutex<LeanLspMcp>>> = LeanLspMcp::spawn(&project_dir)
        .map(|client| {
            eprintln!("[lsp-mcp] Spawned lean-lsp-mcp for session {session_id}");
            Arc::new(Mutex::new(client))
        })
        .map_err(|e| eprintln!("[lsp-mcp] Not available (tools will use fallback): {e}"))
        .ok();

    for iteration in 0..MAX_TOOL_ITERATIONS {
        let _ = tx.send(AppEvent::ToolLoopIteration(iteration));

        let tx_for_events = tx.clone();
        let turn_result = run_codex_turn_with_events(
            CodexTurnRequest {
                session_id,
                messages: &messages,
                model: "gpt-5.4",
                reasoning_effort: "high",
            include_tools: true,
            },
            move |event| match event {
                StreamEvent::TextDelta(delta) => {
                    let _ = tx_for_events.send(AppEvent::StreamDelta(delta));
                }
                StreamEvent::Reasoning => {
                    let _ = tx_for_events.send(AppEvent::ReasoningStarted);
                }
                StreamEvent::ToolCallStart { ref name, .. } => {
                    let _ = tx_for_events.send(AppEvent::AppendNotice {
                        title: "Tool".to_string(),
                        content: format!("Calling {name}..."),
                    });
                }
                _ => {}
            },
        )
        .await;

        match turn_result {
            Ok(result) => {
                // If there are no tool calls, we are done.
                if result.tool_calls.is_empty() {
                    // Text was streamed via StreamDelta events.
                    break;
                }

                turn_used_tools = true;

                // Flush any accumulated streaming text before tool call entries.
                if !result.text.trim().is_empty() {
                    let _ = tx.send(AppEvent::StreamFinished);
                }

                // Add the model's function_call items to the conversation
                // (Responses API requires these before function_call_output)
                for call in &result.tool_calls {
                    messages.push(TurnMessage::FunctionCall {
                        call_id: call.call_id.clone(),
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                    });
                }

                // Execute each tool call.
                let imports = session.proof.imports.clone();
                for call in &result.tool_calls {
                    // Emit tool call event for transcript.
                    eprintln!("[tool] {} ({})", call.name, call.arguments.chars().take(80).collect::<String>());
                    let _ = tx.send(AppEvent::ToolCallReceived {
                        call_id: call.call_id.clone(),
                        tool_name: call.name.clone(),
                        arguments: call.arguments.clone(),
                    });

                    // corpus_search is handled specially (needs store + async cloud client)
                    let output = if call.name == "corpus_search" {
                        let query = serde_json::from_str::<serde_json::Value>(&call.arguments)
                            .ok()
                            .and_then(|v| v.get("query").and_then(|q| q.as_str()).map(str::to_string))
                            .unwrap_or_default();
                        let mut results = Vec::new();

                        // Local FTS search
                        if let Ok(local_hits) = store.search_verified_corpus(&query, 10) {
                            for (label, statement, _vis) in &local_hits {
                                results.push(format!("- {label} :: {statement}"));
                            }
                        }

                        // Cloud semantic search
                        let cloud_client = openproof_cloud::CloudCorpusClient::new(Default::default());
                        eprintln!("[corpus] Cloud available: {:?}", cloud_client.availability());
                        match cloud_client.search_semantic(&query, 10).await {
                            Ok(semantic_hits) => {
                                for hit in &semantic_hits {
                                    let line = format!("- {} (sim:{:.2}) :: {}", hit.label, hit.score, hit.statement);
                                    if !results.iter().any(|r| r.contains(&hit.label)) {
                                        results.push(line);
                                    }
                                }
                            }
                            Err(e) => {
                                eprintln!("[corpus] Cloud semantic search error: {e}");
                            }
                        }

                        if results.is_empty() {
                            ToolOutput { success: true, content: "No results found.".to_string() }
                        } else {
                            ToolOutput { success: true, content: results.join("\n") }
                        }
                    } else {
                        // All other tools: execute on a blocking thread
                        tokio::task::spawn_blocking({
                            let name = call.name.clone();
                            let arguments = call.arguments.clone();
                            let project_dir = project_dir.clone();
                            let workspace_dir = workspace_dir.clone();
                            let imports = imports.clone();
                            let lsp_handle = lsp_mcp.clone();
                            move || {
                                let ctx = ToolContext {
                                    project_dir: &project_dir,
                                    workspace_dir: &workspace_dir,
                                    imports: &imports,
                                    lsp_mcp: lsp_handle,
                                };
                                execute_tool(&name, &arguments, &ctx)
                            }
                        })
                        .await
                        .unwrap_or_else(|_| ToolOutput {
                            success: false,
                            content: "Tool execution panicked".to_string(),
                        })
                    };

                    // Track lean_verify success for workspace sync.
                    // Only mark verified if the LAST verify passed AND no verify failed with sorry.
                    if call.name == "lean_verify" {
                        if !output.success {
                            last_verify_ok = false;
                        } else if !output.content.contains("sorry") {
                            last_verify_ok = true;
                        }
                    }

                    // Emit tool result event for transcript.
                    eprintln!("[tool] {} -> {} ({})",
                        call.name,
                        if output.success { "ok" } else { "FAIL" },
                        output.content.chars().take(120).collect::<String>());
                    let _ = tx.send(AppEvent::ToolResultReceived {
                        call_id: call.call_id.clone(),
                        tool_name: call.name.clone(),
                        success: output.success,
                        output: output.content.clone(),
                    });

                    // Append the tool result to messages for the next API call.
                    messages.push(TurnMessage::tool_result(
                        &call.call_id,
                        &output.content,
                    ));
                }
                // Continue the loop: call the API again with tool results.
            }
            Err(error) => {
                let _ = tx.send(AppEvent::AppendNotice {
                    title: "Assistant Error".to_string(),
                    content: error.to_string(),
                });
                break;
            }
        }
    }

    // After tool loop: sync workspace content to node.content.
    // Read all .lean files from workspace (model may use Main.lean, Defs.lean, etc.)
    if turn_used_tools {
        let workspace_dir = store.workspace_dir(session_id);
        let mut all_lean = String::new();
        if let Ok(files) = store.list_workspace_files(session_id) {
            for (path, _) in &files {
                if path.ends_with(".lean") && !path.contains("history/") {
                    if let Ok(content) = std::fs::read_to_string(workspace_dir.join(path)) {
                        if !all_lean.is_empty() {
                            all_lean.push_str("\n\n");
                        }
                        all_lean.push_str(&content);
                    }
                }
            }
        }
        // Fallback: try Scratch.lean directly
        if all_lean.is_empty() {
            if let Some(scratch) = store.read_scratch(session_id) {
                all_lean = scratch;
            }
        }
        if !all_lean.trim().is_empty() {
            let _ = tx.send(AppEvent::WorkspaceContentSync {
                content: all_lean,
                verified: last_verify_ok,
            });
        }
    }

    let _ = tx.send(AppEvent::TurnFinished);
}

pub fn start_agent_branch_turn(
    tx: mpsc::UnboundedSender<AppEvent>,
    store: AppStore,
    role: AgentRole,
    title: String,
    branch_id: String,
    _task_id: String,
    session_snapshot: SessionSnapshot,
) {
    tokio::spawn(async move {
        let messages =
            build_branch_turn_messages(&store, &session_snapshot, role, &title, &branch_id).await;

        // Branch turns get the full agentic tool loop (file_write, lean_verify, etc.)
        let project_dir = resolve_lean_project_dir();
        let workspace_dir = store.workspace_dir(&session_snapshot.id);
        let _ = std::fs::create_dir_all(&workspace_dir);
        let imports = session_snapshot.proof.imports.clone();
        let mut all_messages = messages;
        let mut accumulated_text = String::new();
        let mut turn_used_tools = false;
        let mut last_verify_ok = false;

        // Spawn lean-lsp-mcp for this branch turn (shared with main loop via Arc).
        let branch_lsp_mcp: Option<Arc<Mutex<LeanLspMcp>>> = LeanLspMcp::spawn(&project_dir)
            .map(|c| Arc::new(Mutex::new(c)))
            .ok();

        for _iteration in 0..MAX_TOOL_ITERATIONS {
            let result = run_codex_turn_with_events(
                CodexTurnRequest {
                    session_id: &branch_id,
                    messages: &all_messages,
                    model: "gpt-5.4",
                    reasoning_effort: "high",
                    include_tools: true,
                },
                |_| {},
            )
            .await;

            match result {
                Ok(turn) => {
                    accumulated_text.push_str(&turn.text);
                    if turn.tool_calls.is_empty() {
                        break;
                    }
                    turn_used_tools = true;
                    // Add function_call items THEN tool results (required by Responses API)
                    for call in &turn.tool_calls {
                        all_messages.push(TurnMessage::FunctionCall {
                            call_id: call.call_id.clone(),
                            name: call.name.clone(),
                            arguments: call.arguments.clone(),
                        });
                    }
                    for call in &turn.tool_calls {
                        let output = if call.name == "corpus_search" {
                            let query = serde_json::from_str::<serde_json::Value>(&call.arguments)
                                .ok()
                                .and_then(|v| v.get("query").and_then(|q| q.as_str()).map(str::to_string))
                                .unwrap_or_default();
                            let mut results = Vec::new();
                            if let Ok(hits) = store.search_verified_corpus(&query, 10) {
                                for (label, statement, _) in &hits {
                                    results.push(format!("- {label} :: {statement}"));
                                }
                            }
                            let cloud = openproof_cloud::CloudCorpusClient::new(Default::default());
                            if let Ok(hits) = cloud.search_semantic(&query, 10).await {
                                for h in &hits {
                                    if !results.iter().any(|r| r.contains(&h.label)) {
                                        results.push(format!("- {} (sim:{:.2}) :: {}", h.label, h.score, h.statement));
                                    }
                                }
                            }
                            ToolOutput {
                                success: true,
                                content: if results.is_empty() { "No results.".to_string() } else { results.join("\n") },
                            }
                        } else {
                            tokio::task::spawn_blocking({
                                let name = call.name.clone();
                                let arguments = call.arguments.clone();
                                let project_dir = project_dir.clone();
                                let workspace_dir = workspace_dir.clone();
                                let imports = imports.clone();
                                let lsp_handle = branch_lsp_mcp.clone();
                                move || {
                                    let ctx = ToolContext {
                                        project_dir: &project_dir,
                                        workspace_dir: &workspace_dir,
                                        imports: &imports,
                                        lsp_mcp: lsp_handle,
                                    };
                                    execute_tool(&name, &arguments, &ctx)
                                }
                            })
                            .await
                            .unwrap_or_else(|_| ToolOutput {
                                success: false,
                                content: "Tool execution panicked".to_string(),
                            })
                        };
                        // Track lean_verify success -- only true if no verify failed with sorry
                        if call.name == "lean_verify" {
                            if !output.success {
                                last_verify_ok = false;
                            } else if !output.content.contains("sorry") {
                                last_verify_ok = true;
                            }
                        }
                        all_messages.push(TurnMessage::tool_result(
                            &call.call_id,
                            &output.content,
                        ));
                    }
                }
                Err(error) => {
                    let message = error.to_string();
                    let _ = tx.send(AppEvent::FinishBranch {
                        branch_id,
                        status: AgentStatus::Error,
                        summary: format!("Branch failed: {}", truncate(&message, 160)),
                        output: message,
                    });
                    return;
                }
            }
        }

        // After tool loop: sync workspace content to node.content
        if turn_used_tools {
            let ws_dir = store.workspace_dir(&session_snapshot.id);
            let mut all_lean = String::new();
            if let Ok(files) = store.list_workspace_files(&session_snapshot.id) {
                for (path, _) in &files {
                    if path.ends_with(".lean") && !path.contains("history/") {
                        if let Ok(content) = std::fs::read_to_string(ws_dir.join(path)) {
                            if !all_lean.is_empty() {
                                all_lean.push_str("\n\n");
                            }
                            all_lean.push_str(&content);
                        }
                    }
                }
            }
            if all_lean.is_empty() {
                if let Some(scratch) = store.read_scratch(&session_snapshot.id) {
                    all_lean = scratch;
                }
            }
            if !all_lean.trim().is_empty() {
                let _ = tx.send(AppEvent::WorkspaceContentSync {
                    content: all_lean,
                    verified: last_verify_ok,
                });
            }
        }

        let content = if accumulated_text.trim().is_empty() {
            "Branch completed tool loop.".to_string()
        } else {
            accumulated_text
        };
        let summary = summarize_branch_output(&content);
        let _ = tx.send(AppEvent::AppendBranchAssistant {
            branch_id: branch_id.clone(),
            content,
            used_tools: turn_used_tools,
        });
        let _ = tx.send(AppEvent::FinishBranch {
            branch_id,
            status: AgentStatus::Done,
            summary,
            output: String::new(),
        });
    });
}

pub fn start_branch_verification(
    tx: mpsc::UnboundedSender<AppEvent>,
    store: AppStore,
    session_snapshot: SessionSnapshot,
    branch_id: String,
    promote: bool,
) {
    let Some((verification_session, focus_node_id)) =
        build_branch_verification_session(&session_snapshot, &branch_id)
    else {
        let _ = tx.send(AppEvent::AppendNotice {
            title: "Verify Error".to_string(),
            content: format!("Branch {branch_id} has no Lean candidate to verify."),
        });
        return;
    };

    let _ = tx.send(AppEvent::LeanVerifyStarted);
    let project_dir = resolve_lean_project_dir();

    let session_id = session_snapshot.id.clone();
    let persistent_scratch = store
        .write_scratch(
            &session_id,
            &openproof_lean::render_node_scratch(
                &verification_session,
                verification_session
                    .proof
                    .nodes
                    .iter()
                    .find(|n| {
                        Some(n.id.as_str())
                            == verification_session.proof.active_node_id.as_deref()
                    })
                    .unwrap_or(&verification_session.proof.nodes[0]),
            ),
        )
        .ok()
        .map(|(path, _)| path);

    tokio::spawn(async move {
        let scratch = persistent_scratch.clone();
        let verification_clone = verification_session.clone();
        let result = tokio::task::spawn_blocking(move || {
            openproof_lean::verify_node_at(
                &project_dir,
                &verification_clone,
                verification_clone
                    .proof
                    .nodes
                    .iter()
                    .find(|n| {
                        Some(n.id.as_str())
                            == verification_clone.proof.active_node_id.as_deref()
                    })
                    .unwrap_or(&verification_clone.proof.nodes[0]),
                scratch.as_deref(),
            )
        })
        .await
        .ok()
        .and_then(Result::ok);
        match result {
            Some(result) => {
                let persist_store = store.clone();
                let index_store = store.clone();
                let persist_session = verification_session.clone();
                let persist_result = result.clone();
                let persist_tx = tx.clone();
                let embed_session = verification_session.clone();
                let embed_ok = result.ok;
                tokio::spawn(async move {
                    let persisted = tokio::task::spawn_blocking(move || {
                        persist_store
                            .record_verification_result(&persist_session, &persist_result)
                    })
                    .await
                    .ok()
                    .and_then(Result::ok);
                    if persisted.is_none() {
                        let _ = persist_tx.send(AppEvent::AppendNotice {
                            title: "Verification Store Error".to_string(),
                            content: "Could not persist branch verification.".to_string(),
                        });
                    }
                    // Embed + index verified items (fire-and-forget)
                    if embed_ok {
                        if let Some(node) = embed_session.proof.active_node_id.as_deref()
                            .and_then(|id| embed_session.proof.nodes.iter().find(|n| n.id == id))
                        {
                            let ik = format!("session/{}/{}", embed_session.id, node.id);
                            crate::helpers::embed_verified_item(
                                ik.clone(),
                                node.label.clone(),
                                node.statement.clone(),
                                format!("{:?}", node.kind).to_lowercase(),
                                String::new(),
                                node.content.clone(),
                            );
                            crate::helpers::index_verified_item(
                                index_store.clone(),
                                ik,
                                String::new(), // module name not easily available here
                            );
                        }
                    }
                });
                let _ = tx.send(AppEvent::BranchVerifyFinished {
                    branch_id,
                    focus_node_id,
                    promote,
                    result,
                });
            }
            None => {
                let _ = tx.send(AppEvent::AppendNotice {
                    title: "Verify Error".to_string(),
                    content: format!("Lean verification crashed for branch {branch_id}."),
                });
            }
        }
    });
}

fn build_branch_verification_session(
    session: &SessionSnapshot,
    branch_id: &str,
) -> Option<(SessionSnapshot, Option<String>)> {
    let branch = session
        .proof
        .branches
        .iter()
        .find(|branch| branch.id == branch_id)?;
    if branch.lean_snippet.trim().is_empty() {
        return None;
    }
    let focus_node_id = branch
        .focus_node_id
        .clone()
        .or_else(|| session.proof.active_node_id.clone())?;
    let mut verification_session = session.clone();
    verification_session.proof.active_node_id = Some(focus_node_id.clone());
    if let Some(node) = verification_session
        .proof
        .nodes
        .iter_mut()
        .find(|node| node.id == focus_node_id)
    {
        node.content = branch.lean_snippet.clone();
    } else {
        return None;
    }
    Some((verification_session, Some(focus_node_id)))
}

pub fn ensure_hidden_agent_branch(
    tx: mpsc::UnboundedSender<AppEvent>,
    store: AppStore,
    state: &mut AppState,
    role: AgentRole,
    title: &str,
    description: &str,
) -> Result<(String, SessionSnapshot), String> {
    let existing_id = state.current_session().and_then(|session| {
        session
            .proof
            .branches
            .iter()
            .filter(|branch| branch.hidden && branch.role == role)
            .max_by(|left, right| left.updated_at.cmp(&right.updated_at))
            .map(|branch| branch.id.clone())
    });

    if let Some(branch_id) = existing_id {
        let now = chrono::Utc::now().to_rfc3339();
        if let Some(session) = state.current_session_mut() {
            if let Some(branch) = session
                .proof
                .branches
                .iter_mut()
                .find(|branch| branch.id == branch_id)
            {
                branch.title = title.to_string();
                branch.hidden = true;
                branch.branch_kind = format!("{}_hidden", agent_role_label(role));
                branch.status = AgentStatus::Running;
                branch.queue_state = BranchQueueState::Running;
                branch.phase = Some(branch_phase_for_role(role).to_string());
                branch.goal_summary = description.to_string();
                branch.search_status = format!("{} branch restarted", agent_role_label(role));
                branch.progress_kind = Some(
                    match role {
                        AgentRole::Planner => "planning",
                        AgentRole::Retriever => "retrieving",
                        AgentRole::Repairer => "repairing",
                        AgentRole::Prover => "candidate",
                        AgentRole::Critic => "blocked",
                    }
                    .to_string(),
                );
                branch.summary = description.to_string();
                branch.updated_at = now.clone();
            }
            session.updated_at = now;
        }
        persist_current_session(
            tx,
            store,
            state,
            format!("Restarted {} branch.", agent_role_label(role)),
        );
        let snapshot = state
            .current_session()
            .cloned()
            .ok_or_else(|| "No active session.".to_string())?;
        return Ok((branch_id, snapshot));
    }

    let (write, branch_id, _task_id) =
        state.spawn_agent_branch(role, title, description, true)?;
    let snapshot = write.session.clone();
    persist_write(tx, store, write);
    Ok((branch_id, snapshot))
}

pub fn submit_selected_question_option(
    tx: mpsc::UnboundedSender<AppEvent>,
    store: AppStore,
    state: &mut AppState,
) {
    let Some(option) = state.selected_question_option().cloned() else {
        emit_local_notice(
            tx,
            state,
            store,
            "Answer Error",
            "No clarification option is currently selectable.".to_string(),
        );
        return;
    };
    let reply_text = if option.formal_target.trim().is_empty() {
        option.label.clone()
    } else {
        option.formal_target.clone()
    };
    if let Some(submitted) = state.submit_text(reply_text) {
        persist_write(
            tx.clone(),
            store.clone(),
            PendingWrite {
                session: submitted.session_snapshot.clone(),
            },
        );
        handle_submission(tx, store, state, submitted);
    }
}

pub fn persist_verification_result(
    tx: mpsc::UnboundedSender<AppEvent>,
    store: AppStore,
    session: SessionSnapshot,
    result: openproof_protocol::LeanVerificationSummary,
) {
    tokio::spawn(async move {
        let store2 = store.clone();
        let result2 = result.clone();
        let session2 = session.clone();
        let outcome =
            tokio::task::spawn_blocking(move || {
                let res = store.record_verification_result(&session, &result);
                if result.ok {
                    crate::helpers::populate_knowledge_graph(&store2, &session.id);
                }
                res
            })
                .await;
        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let _ = tx.send(AppEvent::AppendNotice {
                    title: "Verification Store Error".to_string(),
                    content: format!("Could not persist: {e}"),
                });
            }
            Err(e) => {
                let _ = tx.send(AppEvent::AppendNotice {
                    title: "Verification Store Error".to_string(),
                    content: format!("Task panicked: {e}"),
                });
            }
        }
    });
}
