//! MCP client for lean-lsp-mcp: structured Lean interaction via LSP.
//!
//! Spawns `uvx lean-lsp-mcp` as a child process and communicates via
//! JSON-RPC 2.0 over stdin/stdout (newline-delimited JSON).

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::goal_state::{DiagnosticsResult, GoalState, MultiAttemptResult};

/// Timeout for a single MCP tool call.
const TOOL_TIMEOUT_SECS: u64 = 180;

/// Client for the lean-lsp-mcp MCP server.
pub struct LeanLspMcp {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    next_id: AtomicU64,
    #[allow(dead_code)]
    project_dir: PathBuf,
}

impl LeanLspMcp {
    /// Spawn `uvx lean-lsp-mcp` for the given Lean project directory.
    pub fn spawn(project_dir: &Path) -> Result<Self> {
        let mut child = Command::new("uvx")
            .arg("lean-lsp-mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env("LEAN_PROJECT_PATH", project_dir)
            .spawn()
            .context("Failed to spawn lean-lsp-mcp. Is it installed? Run: uvx lean-lsp-mcp")?;

        let stdin = child.stdin.take().context("No stdin on child")?;
        let stdout = child.stdout.take().context("No stdout on child")?;
        let reader = BufReader::new(stdout);

        let mut client = Self {
            child,
            stdin,
            reader,
            next_id: AtomicU64::new(1),
            project_dir: project_dir.to_path_buf(),
        };

        client.initialize()?;
        Ok(client)
    }

    /// Check if lean-lsp-mcp is available on the system.
    pub fn is_available() -> bool {
        Command::new("uvx")
            .args(["lean-lsp-mcp", "--help"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Perform the MCP initialize handshake.
    fn initialize(&mut self) -> Result<()> {
        let init_request = json!({
            "jsonrpc": "2.0",
            "id": self.next_id(),
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "openproof",
                    "version": "0.1.0"
                }
            }
        });

        self.send(&init_request)?;
        let _response = self.recv()?;

        // Send initialized notification (no id = notification)
        let notification = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });
        self.send(&notification)?;

        Ok(())
    }

    /// Get proof goals at a position in a Lean file.
    /// If column is None, returns goals_before and goals_after.
    pub fn get_goals(
        &mut self,
        file_path: &Path,
        line: usize,
        column: Option<usize>,
    ) -> Result<GoalState> {
        let mut args = json!({
            "file_path": file_path.to_string_lossy(),
            "line": line,
        });
        if let Some(col) = column {
            args["column"] = json!(col);
        }

        let result = self.call_tool("lean_goal", args)?;
        let text = extract_text_content(&result)?;
        serde_json::from_str(&text).context("Failed to parse GoalState from lean_goal response")
    }

    /// Try multiple tactics at a proof position without modifying the file.
    /// Returns goal state and diagnostics for each tactic.
    pub fn screen_tactics(
        &mut self,
        file_path: &Path,
        line: usize,
        column: Option<usize>,
        tactics: &[String],
    ) -> Result<MultiAttemptResult> {
        let mut args = json!({
            "file_path": file_path.to_string_lossy(),
            "line": line,
            "snippets": tactics,
        });
        if let Some(col) = column {
            args["column"] = json!(col);
        }

        let result = self.call_tool("lean_multi_attempt", args)?;
        let text = extract_text_content(&result)?;
        serde_json::from_str(&text)
            .context("Failed to parse MultiAttemptResult from lean_multi_attempt response")
    }

    /// Get compiler diagnostics for a Lean file.
    pub fn get_diagnostics(&mut self, file_path: &Path) -> Result<DiagnosticsResult> {
        let args = json!({
            "file_path": file_path.to_string_lossy(),
        });

        let result = self.call_tool("lean_diagnostic_messages", args)?;
        let text = extract_text_content(&result)?;
        serde_json::from_str(&text)
            .context("Failed to parse DiagnosticsResult")
    }

    /// Get hover info (type signature, documentation) at a position.
    pub fn hover(
        &mut self,
        file_path: &Path,
        line: usize,
        column: usize,
    ) -> Result<Option<String>> {
        let args = json!({
            "file_path": file_path.to_string_lossy(),
            "line": line,
            "column": column,
        });

        let result = self.call_tool("lean_hover_info", args)?;
        let text = extract_text_content(&result)?;
        let value: Value = serde_json::from_str(&text)?;
        Ok(value.get("info").and_then(|v| v.as_str()).map(String::from))
    }

    /// Whether the child process is still running.
    pub fn is_alive(&mut self) -> bool {
        self.child.try_wait().ok().flatten().is_none()
    }

    /// Kill the child process.
    pub fn close(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    // -- Internal --

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Call an MCP tool and return the raw JSON-RPC result.
    fn call_tool(&mut self, tool_name: &str, arguments: Value) -> Result<Value> {
        let request = json!({
            "jsonrpc": "2.0",
            "id": self.next_id(),
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": arguments,
            }
        });

        self.send(&request)?;
        let response = self.recv_timeout(Duration::from_secs(TOOL_TIMEOUT_SECS))?;

        if let Some(error) = response.get("error") {
            let msg = error
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown MCP error");
            bail!("MCP tool '{tool_name}' error: {msg}");
        }

        response
            .get("result")
            .cloned()
            .context("MCP response missing 'result' field")
    }

    /// Send a JSON-RPC message (newline-delimited).
    fn send(&mut self, msg: &Value) -> Result<()> {
        let line = serde_json::to_string(msg)?;
        writeln!(self.stdin, "{line}")?;
        self.stdin.flush()?;
        Ok(())
    }

    /// Read the next JSON-RPC response, skipping notifications.
    fn recv(&mut self) -> Result<Value> {
        self.recv_timeout(Duration::from_secs(TOOL_TIMEOUT_SECS))
    }

    /// Read the next JSON-RPC response with a timeout, skipping notifications.
    fn recv_timeout(&mut self, timeout: Duration) -> Result<Value> {
        let deadline = std::time::Instant::now() + timeout;
        let mut line = String::new();

        loop {
            if std::time::Instant::now() > deadline {
                bail!("MCP response timed out after {}s", timeout.as_secs());
            }

            line.clear();
            let bytes_read = self.reader.read_line(&mut line)?;
            if bytes_read == 0 {
                bail!("MCP server closed stdout (process exited)");
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let value: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => continue, // skip malformed lines (e.g. stderr leak)
            };

            // Skip notifications (no "id" field)
            if value.get("id").is_some() {
                return Ok(value);
            }
            // Otherwise it's a notification -- keep reading
        }
    }
}

impl Drop for LeanLspMcp {
    fn drop(&mut self) {
        self.close();
    }
}

/// Extract the text content from an MCP tool result.
/// MCP returns `{"content": [{"type": "text", "text": "..."}], "isError": false}`.
fn extract_text_content(result: &Value) -> Result<String> {
    // Check for error
    if result.get("isError").and_then(|v| v.as_bool()).unwrap_or(false) {
        let text = result
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.first())
            .and_then(|item| item.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or("unknown error");
        bail!("MCP tool returned error: {text}");
    }

    let content = result
        .get("content")
        .and_then(|c| c.as_array())
        .context("MCP result missing 'content' array")?;

    let mut parts = Vec::new();
    for item in content {
        if item.get("type").and_then(|t| t.as_str()) == Some("text") {
            if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                parts.push(text.to_string());
            }
        }
    }

    if parts.is_empty() {
        bail!("MCP result contained no text content");
    }

    Ok(parts.join("\n"))
}
