//! Structured goal state types for tactic-level Lean interaction.

use serde::{Deserialize, Serialize};

/// A proof goal state at a specific position in a Lean file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalState {
    /// The source line where goals were queried.
    pub line_context: String,
    /// Goals at a specific column position.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goals: Option<Vec<String>>,
    /// Goals at line start (when column omitted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goals_before: Option<Vec<String>>,
    /// Goals at line end (when column omitted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goals_after: Option<Vec<String>>,
}

/// Result of trying a single tactic snippet via multi-attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptResult {
    /// The tactic snippet that was tried.
    pub snippet: String,
    /// Goal list after applying the snippet. Empty if the goal was closed.
    #[serde(default)]
    pub goals: Vec<String>,
    /// Diagnostics (errors/warnings) from this attempt.
    #[serde(default)]
    pub diagnostics: Vec<DiagnosticMessage>,
}

impl AttemptResult {
    /// Whether this tactic succeeded (no error diagnostics).
    pub fn succeeded(&self) -> bool {
        !self.diagnostics.iter().any(|d| d.severity == "error")
    }

    /// Whether this tactic closed the goal (succeeded with no remaining goals).
    pub fn is_solved(&self) -> bool {
        self.succeeded() && self.goals.is_empty()
    }
}

/// Result of trying multiple tactics at a position.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiAttemptResult {
    /// Results for each attempted tactic.
    #[serde(default)]
    pub items: Vec<AttemptResult>,
}

/// A compiler diagnostic message from Lean.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticMessage {
    /// Severity: "error", "warning", "info", or "hint".
    pub severity: String,
    /// Diagnostic message text.
    pub message: String,
    /// Line number (1-indexed).
    pub line: usize,
    /// Column number (1-indexed).
    pub column: usize,
}

/// Diagnostics result from the MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticsResult {
    /// Whether the build succeeded.
    #[serde(default)]
    pub success: bool,
    /// Diagnostic items.
    #[serde(default)]
    pub items: Vec<DiagnosticMessage>,
    /// Paths of failed dependencies (if any).
    #[serde(default)]
    pub failed_dependencies: Vec<String>,
}
