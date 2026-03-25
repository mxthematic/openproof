//! Lean verification: compile files via `lake env lean` and check results.

use anyhow::{Context, Result};
use chrono::Utc;
use openproof_protocol::{LeanHealth, LeanVerificationSummary, ProofNode, SessionSnapshot};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::render::render_node_scratch;

pub fn detect_lean_health(project_dir: &Path) -> Result<LeanHealth> {
    let lean_output = Command::new("lean").arg("--version").output();
    let lake_output = Command::new("lake").arg("--version").output();

    let lean_version = lean_output
        .as_ref()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout.clone()).ok())
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty());
    let lake_version = lake_output
        .as_ref()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout.clone()).ok())
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty());

    Ok(LeanHealth {
        ok: lean_version.is_some() && lake_version.is_some(),
        project_dir: Some(project_dir.display().to_string()),
        lean_version,
        lake_version,
        detail: None,
    })
}

pub fn verify_active_node(
    project_dir: &Path,
    session: &SessionSnapshot,
) -> Result<LeanVerificationSummary> {
    let Some(active_node_id) = session.proof.active_node_id.as_deref() else {
        return Ok(failed_result(
            project_dir,
            String::new(),
            PathBuf::new(),
            "No active proof node is focused.".to_string(),
            Some("no-active-node".to_string()),
        ));
    };
    let Some(node) = session
        .proof
        .nodes
        .iter()
        .find(|node| node.id == active_node_id)
    else {
        return Ok(failed_result(
            project_dir,
            String::new(),
            PathBuf::new(),
            format!("Focused proof node was not found: {active_node_id}"),
            Some("missing-active-node".to_string()),
        ));
    };
    verify_node(project_dir, session, node)
}

pub fn verify_node(
    project_dir: &Path,
    session: &SessionSnapshot,
    node: &ProofNode,
) -> Result<LeanVerificationSummary> {
    verify_node_at(project_dir, session, node, None)
}

/// Verify a node, optionally writing to a persistent scratch path.
/// If `persistent_path` is Some, writes to that path instead of a temp file.
pub fn verify_node_at(
    project_dir: &Path,
    session: &SessionSnapshot,
    node: &ProofNode,
    persistent_path: Option<&Path>,
) -> Result<LeanVerificationSummary> {
    if node.content.trim().is_empty() {
        return Ok(failed_result(
            project_dir,
            String::new(),
            PathBuf::new(),
            format!(
                "No verifiable Lean code is attached to {}. Ask the model for a fenced ```lean``` candidate first.",
                node.label
            ),
            Some("no-verifiable-artifact".to_string()),
        ));
    }

    let rendered_scratch = render_node_scratch(session, node);
    let scratch_path = if let Some(path) = persistent_path {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, &rendered_scratch)?;
        path.to_path_buf()
    } else {
        write_temp_scratch(&rendered_scratch)?
    };
    verify_scratch(project_dir, rendered_scratch, scratch_path)
}

/// Verify a raw Lean snippet by writing it to a temp file and running `lake env lean`.
/// Used by the corpus server for reverification of uploaded items.
pub fn verify_scratch_content(
    project_dir: &Path,
    content: &str,
    namespace: Option<&str>,
    imports: &[String],
) -> Result<LeanVerificationSummary> {
    let import_list = if imports.is_empty() {
        vec!["Mathlib".to_string()]
    } else {
        crate::render::dedup_strings(imports.to_vec())
    };
    let mut lines = Vec::new();
    for import in &import_list {
        lines.push(format!("import {import}"));
    }
    lines.push(String::new());
    if let Some(ns) = namespace {
        if !ns.is_empty() {
            lines.push(format!("namespace {ns}"));
            lines.push(String::new());
        }
    }
    lines.push(content.trim().to_string());
    if let Some(ns) = namespace {
        if !ns.is_empty() {
            lines.push(String::new());
            lines.push(format!("end {ns}"));
        }
    }
    let rendered = lines.join("\n");
    let scratch_path = write_temp_scratch(&rendered)?;
    verify_scratch(project_dir, rendered, scratch_path)
}

pub(crate) fn verify_scratch(
    project_dir: &Path,
    rendered_scratch: String,
    scratch_path: PathBuf,
) -> Result<LeanVerificationSummary> {
    let mathlib_path = project_dir.join(".lake").join("packages").join("mathlib");
    if !mathlib_path.exists() {
        return Ok(failed_result(
            project_dir,
            rendered_scratch,
            scratch_path,
            format!(
                "mathlib is not installed under {}. Run `lake update` in {} first.",
                mathlib_path.display(),
                project_dir.display()
            ),
            Some("mathlib-missing".to_string()),
        ));
    }

    // Use cached LEAN_PATH to call lean directly (saves ~2.5s per call).
    let output = if let Some(lean_path) = crate::tools::resolve_lean_path(project_dir) {
        Command::new("lean")
            .arg("--threads=4")
            .arg(&scratch_path)
            .env("LEAN_PATH", &lean_path)
            .current_dir(project_dir)
            .output()
            .with_context(|| format!("running lean {}", scratch_path.display()))?
    } else {
        Command::new("lake")
            .arg("env")
            .arg("lean")
            .arg(&scratch_path)
            .current_dir(project_dir)
            .output()
            .with_context(|| format!("running lake env lean {}", scratch_path.display()))?
    };

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let sorry_placeholder = contains_sorry_placeholder(&stdout, &stderr);
    Ok(LeanVerificationSummary {
        ok: output.status.success() && !sorry_placeholder,
        code: output.status.code(),
        stdout: stdout.clone(),
        stderr: if !stderr.trim().is_empty() {
            stderr
        } else if !output.status.success() || sorry_placeholder {
            stdout.clone()
        } else {
            String::new()
        },
        error: if sorry_placeholder {
            Some("sorry-placeholder".to_string())
        } else {
            None
        },
        checked_at: Utc::now().to_rfc3339(),
        project_dir: project_dir.display().to_string(),
        scratch_path: scratch_path.display().to_string(),
        rendered_scratch,
    })
}

pub(crate) fn write_temp_scratch(rendered_scratch: &str) -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("openproof-lean-{}", Utc::now().timestamp_millis()));
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let scratch_path = dir.join("Scratch.lean");
    fs::write(&scratch_path, rendered_scratch)
        .with_context(|| format!("writing {}", scratch_path.display()))?;
    Ok(scratch_path)
}

fn failed_result(
    project_dir: &Path,
    rendered_scratch: String,
    scratch_path: PathBuf,
    stderr: String,
    error: Option<String>,
) -> LeanVerificationSummary {
    LeanVerificationSummary {
        ok: false,
        code: None,
        stdout: String::new(),
        stderr,
        error,
        checked_at: Utc::now().to_rfc3339(),
        project_dir: project_dir.display().to_string(),
        scratch_path: scratch_path.display().to_string(),
        rendered_scratch,
    }
}

fn contains_sorry_placeholder(stdout: &str, stderr: &str) -> bool {
    let combined = format!("{stdout}\n{stderr}").to_ascii_lowercase();
    combined.contains("warning: declaration uses 'sorry'")
        || combined.contains("uses 'sorry'")
        || combined.contains("uses sorry")
        || combined.contains("declaration has sorry")
}
