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
}

#[derive(Debug, Clone)]
pub struct TurnMessage {
    pub role: String,
    pub content: String,
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
    json!({
        "model": request.model,
        "store": false,
        "stream": true,
        "instructions": "You are openproof, a local-first formal math assistant. Be concise, direct, and helpful. If the user asks a general question, answer directly. If the user gives a theorem-like statement, help formalize or prove it in Lean.",
        "input": request.messages.iter().map(serialize_turn_message).collect::<Vec<_>>(),
        "include": ["reasoning.encrypted_content"],
        "tool_choice": "auto",
        "tools": [],
        "reasoning": {
            "effort": request.reasoning_effort
        }
    })
}

fn serialize_turn_message(message: &TurnMessage) -> Value {
    if message.role == "assistant" {
        json!({
            "role": "assistant",
            "content": message.content
        })
    } else {
        json!({
            "role": if message.role == "system" { "developer" } else { &message.role },
            "content": [{
                "type": "input_text",
                "text": message.content
            }]
        })
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
    read_event_stream_with_callback(response, |_| {}).await
}

async fn read_event_stream_with_callback(
    response: reqwest::Response,
    on_delta: impl Fn(&str),
) -> Result<String> {
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut full_text = String::new();
    let mut completed_response: Option<Value> = None;

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
                if let Some(delta) = event.get("delta").and_then(Value::as_str).filter(|_| {
                    event.get("type").and_then(Value::as_str) == Some("response.output_text.delta")
                }) {
                    full_text.push_str(delta);
                    on_delta(delta);
                }
                if matches!(
                    event.get("type").and_then(Value::as_str),
                    Some("response.completed" | "response.done")
                ) {
                    if let Some(response_value) = event.get("response") {
                        completed_response = Some(response_value.clone());
                    }
                }
            }
        }
    }

    if full_text.trim().is_empty() {
        if let Some(value) = completed_response {
            return Ok(extract_message_text(&value));
        }
    }

    Ok(full_text)
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
