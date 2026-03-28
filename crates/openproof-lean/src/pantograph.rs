//! Pantograph REPL integration for fast Lean 4 proof interaction.
//!
//! Pantograph keeps the Lean environment loaded in memory, providing:
//! - Fast verification via `frontend.process` (no cold-start import)
//! - Interactive tactic testing via `goal.start` + `goal.tactic`
//! - Structured goal state access
//!
//! Protocol: line-delimited JSON over stdin/stdout.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// A running Pantograph REPL process.
pub struct Pantograph {
    child: Child,
    stdin: std::process::ChildStdin,
    reader: BufReader<std::process::ChildStdout>,
}

/// Result from verifying a Lean file via Pantograph.
pub struct PantographVerifyResult {
    pub ok: bool,
    pub messages: Vec<String>,
    pub has_sorry: bool,
    pub new_constants: Vec<String>,
}

/// Result from testing a tactic.
pub struct TacticTestResult {
    pub success: bool,
    pub remaining_goals: Vec<String>,
    pub error: Option<String>,
    /// Pantograph state ID for the resulting proof state.
    /// Use this to chain further tactics on the new state.
    pub new_state_id: Option<u64>,
}

impl Pantograph {
    /// Spawn a new Pantograph REPL process with the given project's environment.
    pub fn spawn(project_dir: &Path) -> Result<Self> {
        // Find the Pantograph binary
        let repl_path = Self::find_repl(project_dir)?;

        // Get LEAN_PATH from the project
        let lean_path = crate::tools::resolve_lean_path(project_dir)
            .unwrap_or_default();

        // Pass "Mathlib" as argument so Pantograph preloads the environment.
        // This takes ~18s but subsequent operations are milliseconds.
        let mut child = Command::new(&repl_path)
            .arg("Mathlib")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env("LEAN_PATH", &lean_path)
            .current_dir(project_dir)
            .spawn()
            .with_context(|| format!("spawning Pantograph REPL at {}", repl_path.display()))?;

        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let mut reader = BufReader::new(stdout);

        // Wait for "ready." line (Mathlib import takes ~18s)
        let mut ready_line = String::new();
        reader.read_line(&mut ready_line)?;
        if !ready_line.trim().starts_with("ready") {
            anyhow::bail!("Pantograph did not send ready signal: {}", ready_line.trim());
        }

        Ok(Self {
            child,
            stdin,
            reader,
        })
    }

    /// Find the Pantograph REPL binary.
    fn find_repl(project_dir: &Path) -> Result<PathBuf> {
        // Check vendor location first
        let vendor = project_dir
            .join("../vendor/Pantograph/.lake/build/bin/repl")
            .canonicalize();
        if let Ok(path) = vendor {
            if path.exists() {
                return Ok(path);
            }
        }

        // Check if it's on PATH
        if let Ok(output) = Command::new("which").arg("pantograph-repl").output() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Ok(PathBuf::from(path));
            }
        }

        anyhow::bail!("Pantograph REPL not found. Build it: cd vendor/Pantograph && lake build repl")
    }

    /// Send a command and read the JSON response.
    fn send_command(&mut self, cmd: &str, payload: Value) -> Result<Value> {
        let command = json!({ "cmd": cmd, "payload": payload });
        let line = serde_json::to_string(&command)?;

        writeln!(self.stdin, "{}", line)?;
        self.stdin.flush()?;

        let mut response_line = String::new();
        self.reader.read_line(&mut response_line)?;

        serde_json::from_str(&response_line.trim())
            .with_context(|| format!("parsing Pantograph response: {}", response_line.trim()))
    }

    /// Check if the process is still alive.
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Verify a Lean file by processing its content.
    /// Returns diagnostics and whether it compiled successfully.
    pub fn verify_content(&mut self, content: &str) -> Result<PantographVerifyResult> {
        let response = self.send_command("frontend.process", json!({
            "file": content,
            "readHeader": true,
            "inheritEnv": false,
            "newConstants": true,
        }))?;

        let mut messages = Vec::new();
        let mut has_error = false;
        let mut has_sorry = false;
        let mut new_constants = Vec::new();

        if let Some(units) = response.get("units").and_then(|v| v.as_array()) {
            for unit in units {
                if let Some(msgs) = unit.get("messages").and_then(|v| v.as_array()) {
                    for msg in msgs {
                        let severity = msg.get("severity").and_then(|v| v.as_str()).unwrap_or("");
                        let data = msg.get("data").and_then(|v| v.as_str()).unwrap_or("");
                        if severity == "error" {
                            has_error = true;
                        }
                        if data.contains("declaration uses 'sorry'") {
                            has_sorry = true;
                        }
                        messages.push(format!("{}: {}", severity, data));
                    }
                }
                if let Some(consts) = unit.get("newConstants").and_then(|v| v.as_array()) {
                    for c in consts {
                        if let Some(name) = c.as_str() {
                            new_constants.push(name.to_string());
                        }
                    }
                }
            }
        }

        // Also check for error in top-level response
        if let Some(err) = response.get("error").and_then(|v| v.as_str()) {
            has_error = true;
            messages.push(format!("error: {}", err));
        }

        Ok(PantographVerifyResult {
            ok: !has_error && !has_sorry,
            messages,
            has_sorry,
            new_constants,
        })
    }

    /// Start a proof goal from a type expression.
    /// Returns a state ID that can be used with `try_tactic`.
    pub fn start_goal(&mut self, expr: &str) -> Result<Option<u64>> {
        let response = self.send_command("goal.start", json!({
            "expr": expr,
        }))?;

        if let Some(id) = response.get("stateId").and_then(|v| v.as_u64()) {
            Ok(Some(id))
        } else {
            Ok(None)
        }
    }

    /// Try a tactic on a goal state. Returns whether it succeeded
    /// and any remaining goals.
    pub fn try_tactic(&mut self, state_id: u64, goal_id: u64, tactic: &str) -> Result<TacticTestResult> {
        let response = self.send_command("goal.tactic", json!({
            "stateId": state_id,
            "goalId": goal_id,
            "tactic": tactic,
        }))?;

        if let Some(err) = response.get("parseError").and_then(|v| v.as_str()) {
            return Ok(TacticTestResult {
                success: false,
                remaining_goals: Vec::new(),
                error: Some(err.to_string()),
                new_state_id: None,
            });
        }

        if let Some(errors) = response.get("tacticErrors").and_then(|v| v.as_array()) {
            if !errors.is_empty() {
                let err_msgs: Vec<String> = errors.iter()
                    .filter_map(|e| e.as_str().map(String::from))
                    .collect();
                return Ok(TacticTestResult {
                    success: false,
                    remaining_goals: Vec::new(),
                    error: Some(err_msgs.join("; ")),
                    new_state_id: None,
                });
            }
        }

        let new_state_id = response
            .get("nextStateId")
            .or_else(|| response.get("stateId"))
            .and_then(|v| v.as_u64());
        let goals: Vec<String> = response.get("goals")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|g| {
                        // New format: goal is an object with target.pp field
                        g.get("target")
                            .and_then(|t| t.get("pp"))
                            .and_then(|pp| pp.as_str())
                            .map(String::from)
                            // Old format: goal is a plain string
                            .or_else(|| g.as_str().map(String::from))
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(TacticTestResult {
            success: goals.is_empty(),
            remaining_goals: goals,
            error: None,
            new_state_id,
        })
    }

    /// Delete a goal state to free memory.
    pub fn delete_goal(&mut self, state_id: u64) -> Result<()> {
        let _ = self.send_command("goal.delete", json!({ "stateId": state_id }))?;
        Ok(())
    }

    /// Inspect an environment symbol.
    pub fn inspect(&mut self, name: &str) -> Result<Option<String>> {
        let response = self.send_command("env.inspect", json!({
            "name": name,
            "value": false,
        }))?;

        if let Some(ty) = response.get("type").and_then(|v| v.get("pp")).and_then(|v| v.as_str()) {
            Ok(Some(ty.to_string()))
        } else {
            Ok(None)
        }
    }
}

impl Drop for Pantograph {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}
