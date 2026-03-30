pub mod tools;

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use directories::BaseDirs;
use futures_util::StreamExt;
use openproof_protocol::AuthSummary;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

const OPENAI_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const DEFAULT_ORIGINATOR: &str = "codex_cli_rs";

pub fn default_auth_path() -> Result<PathBuf> {
    let base_dirs = BaseDirs::new().context("could not resolve home directory")?;
    Ok(base_dirs.home_dir().join(".openproof").join("auth.json"))
}

pub fn default_codex_auth_path() -> Result<PathBuf> {
    let base_dirs = BaseDirs::new().context("could not resolve home directory")?;
    Ok(base_dirs.home_dir().join(".codex").join("auth.json"))
}

pub fn load_auth_summary() -> Result<AuthSummary> {
    let path = default_auth_path()?;
    load_auth_summary_from_path(path)
}

pub fn load_auth_summary_from_path(path: PathBuf) -> Result<AuthSummary> {
    if !path.exists() {
        return Ok(AuthSummary::default());
    }
    let raw = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let json: Value =
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
    let tokens = json.get("tokens");
    let id_token = tokens.and_then(|item| item.get("idToken"));
    Ok(AuthSummary {
        logged_in: json
            .get("authMode")
            .and_then(Value::as_str)
            .map(|mode| mode == "chatgpt")
            .unwrap_or(false),
        auth_mode: json
            .get("authMode")
            .and_then(Value::as_str)
            .map(str::to_string),
        email: id_token
            .and_then(|item| item.get("email"))
            .and_then(Value::as_str)
            .map(str::to_string),
        plan: id_token
            .and_then(|item| item.get("chatgptPlanType"))
            .and_then(Value::as_str)
            .map(str::to_string),
        account_id: tokens
            .and_then(|item| item.get("accountId"))
            .and_then(Value::as_str)
            .map(str::to_string),
        last_refresh: json
            .get("lastRefresh")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct StoredAuthState {
    pub version: Option<u64>,
    pub auth_mode: Option<String>,
    pub openai_api_key: Option<String>,
    pub tokens: Option<StoredAuthTokens>,
    pub last_refresh: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct StoredAuthTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub account_id: Option<String>,
}

pub fn load_auth_state() -> Result<Option<StoredAuthState>> {
    let path = default_auth_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let parsed = serde_json::from_str::<StoredAuthState>(&raw)
        .with_context(|| format!("parsing {}", path.display()))?;
    Ok(Some(parsed))
}

pub fn sync_auth_from_codex_cli() -> Result<Option<AuthSummary>> {
    let codex_path = default_codex_auth_path()?;
    if !codex_path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&codex_path)
        .with_context(|| format!("reading {}", codex_path.display()))?;
    let json: Value =
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", codex_path.display()))?;
    let auth_mode = json.get("auth_mode").and_then(Value::as_str);
    if auth_mode != Some("chatgpt") {
        return Ok(None);
    }

    let tokens = json.get("tokens").unwrap_or(&Value::Null);
    let id_token = match tokens.get("id_token").and_then(Value::as_str) {
        Some(value) if !value.trim().is_empty() => value.trim(),
        _ => return Ok(None),
    };
    let access_token = match tokens.get("access_token").and_then(Value::as_str) {
        Some(value) if !value.trim().is_empty() => value.trim(),
        _ => return Ok(None),
    };
    let refresh_token = match tokens.get("refresh_token").and_then(Value::as_str) {
        Some(value) if !value.trim().is_empty() => value.trim(),
        _ => return Ok(None),
    };

    let id_payload = parse_jwt_payload(id_token).unwrap_or(Value::Null);
    let auth_claims = id_payload
        .get("https://api.openai.com/auth")
        .cloned()
        .unwrap_or(Value::Null);
    let profile_claims = id_payload
        .get("https://api.openai.com/profile")
        .cloned()
        .unwrap_or(Value::Null);

    let email = string_at(&id_payload, &["email"])
        .or_else(|| string_at(&profile_claims, &["email"]))
        .map(str::to_string);
    let plan = string_at(&auth_claims, &["chatgpt_plan_type"]).map(str::to_string);
    let user_id = string_at(&auth_claims, &["chatgpt_user_id"])
        .or_else(|| string_at(&auth_claims, &["user_id"]))
        .map(str::to_string);
    let account_id = string_at(tokens, &["account_id"])
        .or_else(|| string_at(&auth_claims, &["chatgpt_account_id"]))
        .map(str::to_string);
    let openai_api_key = json
        .get("OPENAI_API_KEY")
        .and_then(Value::as_str)
        .map(str::to_string);
    let last_refresh = json
        .get("last_refresh")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

    let payload = json!({
        "version": 1,
        "authMode": "chatgpt",
        "openaiApiKey": openai_api_key,
        "tokens": {
            "idToken": {
                "rawJwt": id_token,
                "email": email,
                "chatgptPlanType": plan,
                "chatgptUserId": user_id,
                "chatgptAccountId": account_id,
                "exp": numeric_at(&id_payload, &["exp"])
            },
            "accessToken": access_token,
            "refreshToken": refresh_token,
            "accountId": account_id,
            "accessExp": jwt_exp(access_token),
            "idExp": jwt_exp(id_token)
        },
        "lastRefresh": last_refresh
    });

    let target = default_auth_path()?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    fs::write(
        &target,
        format!("{}\n", serde_json::to_string_pretty(&payload)?),
    )
    .with_context(|| format!("writing {}", target.display()))?;
    Ok(Some(load_auth_summary_from_path(target)?))
}

pub struct CodexTurnRequest<'a> {
    pub session_id: &'a str,
    pub messages: &'a [TurnMessage],
    pub model: &'a str,
    pub reasoning_effort: &'a str,
    /// If false, tools are not included in the API call. Default true.
    pub include_tools: bool,
}

/// A message in the conversation sent to the model.
#[derive(Debug, Clone)]
pub enum TurnMessage {
    /// A regular chat message (user, assistant, system/developer).
    Chat { role: String, content: String },
    /// A function call the model made (must be included before the result).
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    /// A tool result returned after executing a tool call.
    ToolResult { call_id: String, output: String },
}

impl TurnMessage {
    /// Convenience constructor for chat messages (backward compatible).
    pub fn chat(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self::Chat {
            role: role.into(),
            content: content.into(),
        }
    }

    /// Convenience constructor for tool results.
    pub fn tool_result(call_id: impl Into<String>, output: impl Into<String>) -> Self {
        Self::ToolResult {
            call_id: call_id.into(),
            output: output.into(),
        }
    }
}

pub async fn run_codex_turn(request: CodexTurnRequest<'_>) -> Result<String> {
    let auth = load_auth_state()?
        .filter(|state| state.auth_mode.as_deref() == Some("chatgpt"))
        .context("Openproof is not authenticated. Run `openproof login`.")?;
    let tokens = auth
        .tokens
        .context("Missing ChatGPT tokens in auth state.")?;

    let client = reqwest::Client::builder().build()?;
    let payload = build_turn_payload(&request);
    let mut response = client
        .post(format!("{OPENAI_CODEX_BASE_URL}/responses"))
        .header("authorization", format!("Bearer {}", tokens.access_token))
        .header("content-type", "application/json")
        .header("accept", "text/event-stream")
        .header("originator", DEFAULT_ORIGINATOR)
        .header(
            "chatgpt-account-id",
            tokens.account_id.clone().unwrap_or_default(),
        )
        .header("session_id", request.session_id)
        .json(&payload)
        .send()
        .await?;

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        let synced = sync_auth_from_codex_cli()?;
        if synced.is_some() {
            let retry_auth = load_auth_state()?
                .filter(|state| state.auth_mode.as_deref() == Some("chatgpt"))
                .context("Openproof is not authenticated after sync.")?;
            let retry_tokens = retry_auth
                .tokens
                .context("Missing ChatGPT tokens in synced auth state.")?;
            response = client
                .post(format!("{OPENAI_CODEX_BASE_URL}/responses"))
                .header(
                    "authorization",
                    format!("Bearer {}", retry_tokens.access_token),
                )
                .header("content-type", "application/json")
                .header("accept", "text/event-stream")
                .header("originator", DEFAULT_ORIGINATOR)
                .header(
                    "chatgpt-account-id",
                    retry_tokens.account_id.clone().unwrap_or_default(),
                )
                .header("session_id", request.session_id)
                .json(&payload)
                .send()
                .await?;
        }
    }

    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().await.unwrap_or_default();
        anyhow::bail!("OpenProof transport failed with status {status}: {detail}");
    }

    read_event_stream(response).await
}

fn build_turn_payload(request: &CodexTurnRequest<'_>) -> Value {
    let tools_value = if request.include_tools {
        let mut all_tools: Vec<Value> = vec![json!({ "type": "web_search" })];
        all_tools.extend(tools::tool_definitions());
        json!(all_tools)
    } else {
        json!([])
    };
    json!({
        "model": request.model,
        "store": false,
        "stream": true,
        "instructions": "You are openproof, a local-first formal math assistant. Be concise, direct, and helpful. If the user asks a general question, answer directly. If the user gives a theorem-like statement, help formalize or prove it in Lean. NEVER refuse to attempt a proof. Do not discuss problem difficulty, sources, or feasibility. Always use tools to write and verify Lean code.",
        "input": request.messages.iter().map(serialize_turn_message).collect::<Vec<_>>(),
        "include": ["reasoning.encrypted_content"],
        "tool_choice": if request.include_tools { "auto" } else { "none" },
        "tools": tools_value,
        "reasoning": {
            "effort": request.reasoning_effort
        }
    })
}

fn serialize_turn_message(message: &TurnMessage) -> Value {
    match message {
        TurnMessage::Chat { role, content } => {
            if role == "assistant" {
                json!({
                    "role": "assistant",
                    "content": content
                })
            } else {
                json!({
                    "role": if role == "system" { "developer" } else { role.as_str() },
                    "content": [{
                        "type": "input_text",
                        "text": content
                    }]
                })
            }
        }
        TurnMessage::FunctionCall {
            call_id,
            name,
            arguments,
        } => {
            json!({
                "type": "function_call",
                "call_id": call_id,
                "name": name,
                "arguments": arguments
            })
        }
        TurnMessage::ToolResult { call_id, output } => {
            json!({
                "type": "function_call_output",
                "call_id": call_id,
                "output": output
            })
        }
    }
}

/// Like `run_codex_turn` but sends each text delta through a channel as it arrives.
/// The final complete text is still returned.
pub async fn run_codex_turn_streaming(
    request: CodexTurnRequest<'_>,
    delta_tx: tokio::sync::mpsc::UnboundedSender<String>,
) -> Result<String> {
    let auth = load_auth_state()?
        .filter(|state| state.auth_mode.as_deref() == Some("chatgpt"))
        .context("Openproof is not authenticated. Run `openproof login`.")?;
    let tokens = auth
        .tokens
        .context("Missing ChatGPT tokens in auth state.")?;

    let client = reqwest::Client::builder().build()?;
    let payload = build_turn_payload(&request);
    let mut response = client
        .post(format!("{OPENAI_CODEX_BASE_URL}/responses"))
        .header("authorization", format!("Bearer {}", tokens.access_token))
        .header("content-type", "application/json")
        .header("accept", "text/event-stream")
        .header("originator", DEFAULT_ORIGINATOR)
        .header(
            "chatgpt-account-id",
            tokens.account_id.clone().unwrap_or_default(),
        )
        .header("session_id", request.session_id)
        .json(&payload)
        .send()
        .await?;

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        let synced = sync_auth_from_codex_cli()?;
        if synced.is_some() {
            let retry_auth = load_auth_state()?
                .filter(|state| state.auth_mode.as_deref() == Some("chatgpt"))
                .context("Openproof is not authenticated after sync.")?;
            let retry_tokens = retry_auth
                .tokens
                .context("Missing ChatGPT tokens in synced auth state.")?;
            response = client
                .post(format!("{OPENAI_CODEX_BASE_URL}/responses"))
                .header(
                    "authorization",
                    format!("Bearer {}", retry_tokens.access_token),
                )
                .header("content-type", "application/json")
                .header("accept", "text/event-stream")
                .header("originator", DEFAULT_ORIGINATOR)
                .header(
                    "chatgpt-account-id",
                    retry_tokens.account_id.clone().unwrap_or_default(),
                )
                .header("session_id", request.session_id)
                .json(&payload)
                .send()
                .await?;
        }
    }

    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().await.unwrap_or_default();
        anyhow::bail!("OpenProof transport failed with status {status}: {detail}");
    }

    read_event_stream_with_callback(response, |delta| {
        let _ = delta_tx.send(delta.to_string());
    })
    .await
}

async fn read_event_stream(response: reqwest::Response) -> Result<String> {
    let result = read_event_stream_with_events(response, |_| {}).await?;
    Ok(result.text)
}

/// A tool call extracted from the model's response.
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub call_id: String,
    pub name: String,
    /// JSON-encoded arguments string.
    pub arguments: String,
}

/// Combined result of a model turn: accumulated text plus any tool calls.
#[derive(Debug, Clone, Default)]
pub struct TurnResult {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
}

/// Status updates sent during streaming.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// A text delta from the model's visible output.
    TextDelta(String),
    /// The model is reasoning (thinking).
    Reasoning,
    /// A tool call has started (output item added).
    ToolCallStart { call_id: String, name: String },
    /// Streaming arguments delta for an in-progress tool call.
    ToolCallArgsDelta { call_id: String, delta: String },
    /// A tool call is complete with its full arguments.
    ToolCallDone {
        call_id: String,
        name: String,
        arguments: String,
    },
}

async fn read_event_stream_with_callback(
    response: reqwest::Response,
    on_delta: impl Fn(&str),
) -> Result<String> {
    let result = read_event_stream_with_events(response, |event| {
        if let StreamEvent::TextDelta(ref delta) = event {
            on_delta(delta);
        }
    })
    .await?;
    Ok(result.text)
}

/// Like run_codex_turn_streaming but also reports reasoning and tool call events.
/// Returns a `TurnResult` with both accumulated text and any tool calls.
pub async fn run_codex_turn_with_events(
    request: CodexTurnRequest<'_>,
    on_event: impl Fn(StreamEvent) + Send + 'static,
) -> Result<TurnResult> {
    let auth = load_auth_state()?
        .filter(|state| state.auth_mode.as_deref() == Some("chatgpt"))
        .context("Openproof is not authenticated. Run `openproof login`.")?;
    let tokens = auth
        .tokens
        .context("Missing ChatGPT tokens in auth state.")?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()?;
    let payload = build_turn_payload(&request);
    let mut response = client
        .post(format!("{OPENAI_CODEX_BASE_URL}/responses"))
        .header("authorization", format!("Bearer {}", tokens.access_token))
        .header("content-type", "application/json")
        .header("accept", "text/event-stream")
        .header("originator", DEFAULT_ORIGINATOR)
        .header(
            "chatgpt-account-id",
            tokens.account_id.clone().unwrap_or_default(),
        )
        .header("session_id", request.session_id)
        .json(&payload)
        .send()
        .await?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        if let Ok(Some(_)) = sync_auth_from_codex_cli() {
            let retry_auth = load_auth_state()?
                .filter(|state| state.auth_mode.as_deref() == Some("chatgpt"))
                .context("Openproof is not authenticated after sync.")?;
            let retry_tokens = retry_auth
                .tokens
                .context("Missing ChatGPT tokens in synced auth state.")?;
            response = client
                .post(format!("{OPENAI_CODEX_BASE_URL}/responses"))
                .header(
                    "authorization",
                    format!("Bearer {}", retry_tokens.access_token),
                )
                .header("content-type", "application/json")
                .header("accept", "text/event-stream")
                .header("originator", DEFAULT_ORIGINATOR)
                .header(
                    "chatgpt-account-id",
                    retry_tokens.account_id.clone().unwrap_or_default(),
                )
                .header("session_id", request.session_id)
                .json(&payload)
                .send()
                .await?;
        }
    }

    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().await.unwrap_or_default();
        anyhow::bail!("OpenProof transport failed with status {status}: {detail}");
    }

    read_event_stream_with_events(response, on_event).await
}

async fn read_event_stream_with_events(
    response: reqwest::Response,
    on_event: impl Fn(StreamEvent),
) -> Result<TurnResult> {
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut full_text = String::new();
    let mut completed_response: Option<Value> = None;
    let mut reasoning_signaled = false;

    // Track in-progress tool calls by their output_index.
    let mut pending_calls: std::collections::HashMap<u64, (String, String, String)> =
        std::collections::HashMap::new(); // output_index -> (call_id, name, args_buffer)
    let mut finished_calls: Vec<ToolCall> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        let normalized = buffer.replace("\r\n", "\n");
        let mut parts = normalized
            .split("\n\n")
            .map(str::to_string)
            .collect::<Vec<_>>();
        buffer = parts.pop().unwrap_or_default();

        for part in parts {
            if let Some(event) = parse_sse_event(&part) {
                let event_type = event.get("type").and_then(Value::as_str).unwrap_or("");

                // Detect reasoning phase events
                if event_type.contains("reasoning") && !reasoning_signaled {
                    reasoning_signaled = true;
                    on_event(StreamEvent::Reasoning);
                }

                // Text output delta
                if let Some(delta) = event
                    .get("delta")
                    .and_then(Value::as_str)
                    .filter(|_| event_type == "response.output_text.delta")
                {
                    full_text.push_str(delta);
                    on_event(StreamEvent::TextDelta(delta.to_string()));
                }

                // Tool call: new output item of type function_call
                if event_type == "response.output_item.added" {
                    if let Some(item) = event.get("item") {
                        let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
                        if item_type == "function_call" {
                            let call_id = item
                                .get("call_id")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            let name = item
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            let output_index = event
                                .get("output_index")
                                .and_then(Value::as_u64)
                                .unwrap_or(0);
                            pending_calls.insert(
                                output_index,
                                (call_id.clone(), name.clone(), String::new()),
                            );
                            on_event(StreamEvent::ToolCallStart { call_id, name });
                        }
                    }
                }

                // Tool call: streaming argument deltas
                if event_type == "response.function_call_arguments.delta" {
                    let output_index = event
                        .get("output_index")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    let delta = event.get("delta").and_then(Value::as_str).unwrap_or("");
                    if let Some(entry) = pending_calls.get_mut(&output_index) {
                        entry.2.push_str(delta);
                        on_event(StreamEvent::ToolCallArgsDelta {
                            call_id: entry.0.clone(),
                            delta: delta.to_string(),
                        });
                    }
                }

                // Tool call: arguments complete
                if event_type == "response.function_call_arguments.done" {
                    let output_index = event
                        .get("output_index")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    if let Some((call_id, name, args)) = pending_calls.remove(&output_index) {
                        let final_args = event
                            .get("arguments")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or(args);
                        on_event(StreamEvent::ToolCallDone {
                            call_id: call_id.clone(),
                            name: name.clone(),
                            arguments: final_args.clone(),
                        });
                        finished_calls.push(ToolCall {
                            call_id,
                            name,
                            arguments: final_args,
                        });
                    }
                }

                if matches!(event_type, "response.completed" | "response.done") {
                    if let Some(response_value) = event.get("response") {
                        completed_response = Some(response_value.clone());
                    }
                }
            }
        }
    }

    // If we got no streamed text but have a completed response, extract text from it.
    // Also extract any tool calls from the completed response if we missed them in streaming.
    if full_text.trim().is_empty() && finished_calls.is_empty() {
        if let Some(ref value) = completed_response {
            full_text = extract_message_text(value);
            finished_calls = extract_tool_calls(value);
        }
    }

    Ok(TurnResult {
        text: full_text,
        tool_calls: finished_calls,
    })
}

fn parse_sse_event(chunk: &str) -> Option<Value> {
    let data = chunk
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n");
    if data.is_empty() || data == "[DONE]" {
        return None;
    }
    serde_json::from_str::<Value>(&data).ok()
}

fn extract_message_text(value: &Value) -> String {
    let Some(output) = value.get("output").and_then(Value::as_array) else {
        return String::new();
    };
    for item in output {
        let Some(item_type) = item.get("type").and_then(Value::as_str) else {
            continue;
        };
        if item_type != "message" {
            continue;
        }
        let Some(content) = item.get("content").and_then(Value::as_array) else {
            continue;
        };
        for part in content {
            if part.get("type").and_then(Value::as_str) == Some("output_text") {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    return text.to_string();
                }
            }
        }
    }
    String::new()
}

/// Extract tool calls from a completed response object (fallback for non-streamed responses).
fn extract_tool_calls(value: &Value) -> Vec<ToolCall> {
    let Some(output) = value.get("output").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut calls = Vec::new();
    for item in output {
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
        if item_type == "function_call" {
            let call_id = item
                .get("call_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let arguments = item
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}")
                .to_string();
            calls.push(ToolCall {
                call_id,
                name,
                arguments,
            });
        }
    }
    calls
}

fn parse_jwt_payload(token: &str) -> Option<Value> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let normalized = payload.replace('-', "+").replace('_', "/");
    let padding_len = (4 - (normalized.len() % 4)) % 4;
    let padded = format!("{normalized}{}", "=".repeat(padding_len));
    let decoded = STANDARD.decode(padded.as_bytes()).ok()?;
    serde_json::from_slice::<Value>(&decoded).ok()
}

fn jwt_exp(token: &str) -> Option<i64> {
    numeric_at(&parse_jwt_payload(token).unwrap_or(Value::Null), &["exp"])
}

fn string_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_str().filter(|item| !item.trim().is_empty())
}

fn numeric_at(value: &Value, path: &[&str]) -> Option<i64> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_i64()
}
