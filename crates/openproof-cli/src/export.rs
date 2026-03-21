//! Paper rendering, file I/O primitives, and session export.
//!
//! These utilities are used by `slash_commands` (for `/paper`, `/export`,
//! `/remember`) and by `system_prompt` (for loading AGENTS.md and memory
//! files).

use anyhow::{bail, Result};
use directories::BaseDirs;
use openproof_protocol::SessionSnapshot;
use std::{fs, path::Path};

// ---------------------------------------------------------------------------
// Paper rendering
// ---------------------------------------------------------------------------

pub fn render_paper_text(session: &SessionSnapshot) -> String {
    let mut lines = vec![
        format!("Title: {}", session.title),
        format!(
            "Problem: {}",
            session
                .proof
                .problem
                .clone()
                .unwrap_or_else(|| "none".to_string())
        ),
        format!(
            "Formal target: {}",
            session
                .proof
                .formal_target
                .clone()
                .unwrap_or_else(|| "none".to_string())
        ),
        format!(
            "Accepted target: {}",
            session
                .proof
                .accepted_target
                .clone()
                .unwrap_or_else(|| "none".to_string())
        ),
        String::new(),
        "Notes:".to_string(),
    ];
    if session.proof.paper_notes.is_empty() {
        lines.push("No paper notes yet.".to_string());
    } else {
        lines.extend(
            session
                .proof
                .paper_notes
                .iter()
                .enumerate()
                .map(|(index, note)| format!("{}. {}", index + 1, note)),
        );
    }
    lines.join("\n")
}

pub fn render_paper_tex(session: &SessionSnapshot) -> String {
    format!(
        "\\section*{{{}}}\n\\textbf{{Problem:}} {}\\\\\n\\textbf{{Formal target:}} {}\\\\\n\\textbf{{Accepted target:}} {}\n\n\\begin{{itemize}}\n{}\n\\end{{itemize}}\n",
        escape_tex(&session.title),
        escape_tex(session.proof.problem.as_deref().unwrap_or("none")),
        escape_tex(session.proof.formal_target.as_deref().unwrap_or("none")),
        escape_tex(session.proof.accepted_target.as_deref().unwrap_or("none")),
        if session.proof.paper_notes.is_empty() {
            "\\item No paper notes yet.".to_string()
        } else {
            session
                .proof
                .paper_notes
                .iter()
                .map(|note| format!("\\item {}", escape_tex(note)))
                .collect::<Vec<_>>()
                .join("\n")
        }
    )
}

pub fn escape_tex(input: &str) -> String {
    input
        .replace('\\', "\\textbackslash{}")
        .replace('{', "\\{")
        .replace('}', "\\}")
        .replace('_', "\\_")
        .replace('&', "\\&")
        .replace('%', "\\%")
        .replace('$', "\\$")
        .replace('#', "\\#")
}

pub fn sanitize_file_stem(input: &str) -> String {
    let mut value = input
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect::<String>();
    while value.contains("__") {
        value = value.replace("__", "_");
    }
    let value = value.trim_matches('_').to_string();
    if value.is_empty() {
        "artifact".to_string()
    } else {
        value
    }
}

pub fn sanitize_workspace_path(path: &Path) -> String {
    let mut value = path
        .to_string_lossy()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect::<String>();
    while value.contains("__") {
        value = value.replace("__", "_");
    }
    value = value.trim_matches('_').to_string();
    if value.is_empty() {
        "workspace".to_string()
    } else {
        value
    }
}

// ---------------------------------------------------------------------------
// File I/O primitives
// ---------------------------------------------------------------------------

pub fn read_text_if_exists(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

pub fn ensure_and_read_text(path: &Path) -> Result<String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if !path.exists() {
        fs::write(path, "")?;
    }
    Ok(fs::read_to_string(path).unwrap_or_default())
}

pub fn append_memory_entry(path: &Path, text: &str) -> Result<String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let line = format!("- {} {}\n", chrono::Utc::now().to_rfc3339(), text.trim());
    let mut content = if path.exists() {
        fs::read_to_string(path).unwrap_or_default()
    } else {
        String::new()
    };
    content.push_str(&line);
    fs::write(path, content)?;
    Ok(line.trim().to_string())
}

// ---------------------------------------------------------------------------
// Session export
// ---------------------------------------------------------------------------

pub fn export_session_artifacts(
    session: Option<&SessionSnapshot>,
    target: &str,
) -> Result<Vec<String>> {
    let Some(session) = session else {
        bail!("No active session.");
    };
    let base_dirs =
        BaseDirs::new().ok_or_else(|| anyhow::anyhow!("could not resolve home directory"))?;
    let export_dir = base_dirs
        .home_dir()
        .join(".openproof")
        .join("exports")
        .join(&session.id);
    fs::create_dir_all(&export_dir)?;

    let mut written = Vec::new();
    if matches!(target, "paper" | "all") {
        let path = export_dir.join("paper.txt");
        fs::write(&path, render_paper_text(session))?;
        written.push(path.display().to_string());
    }
    if matches!(target, "tex" | "all") {
        let path = export_dir.join("paper.tex");
        fs::write(&path, render_paper_tex(session))?;
        written.push(path.display().to_string());
    }
    if matches!(target, "lean" | "all") {
        if let Some(node) = session
            .proof
            .active_node_id
            .as_deref()
            .and_then(|id| session.proof.nodes.iter().find(|node| node.id == id))
        {
            if !node.content.trim().is_empty() {
                let path = export_dir.join(format!("{}.lean", sanitize_file_stem(&node.label)));
                fs::write(&path, node.content.trim())?;
                written.push(path.display().to_string());
            }
        }
        if let Some(branch) = session
            .proof
            .active_branch_id
            .as_deref()
            .and_then(|id| session.proof.branches.iter().find(|branch| branch.id == id))
        {
            if !branch.lean_snippet.trim().is_empty() {
                let path = export_dir
                    .join(format!("{}_branch.lean", sanitize_file_stem(&branch.title)));
                fs::write(&path, branch.lean_snippet.trim())?;
                written.push(path.display().to_string());
            }
        }
    }
    if target == "all" {
        let path = export_dir.join("session.json");
        fs::write(
            &path,
            format!("{}\n", serde_json::to_string_pretty(session)?),
        )?;
        written.push(path.display().to_string());
    }
    if !matches!(target, "paper" | "tex" | "lean" | "all") {
        bail!("Usage: /export paper|tex|lean|all");
    }
    Ok(written)
}
