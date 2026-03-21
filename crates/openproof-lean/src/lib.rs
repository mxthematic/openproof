use anyhow::{Context, Result};
use chrono::Utc;
use openproof_protocol::{
    LeanHealth, LeanVerificationSummary, ProofNode, ProofNodeKind, ProofNodeStatus, SessionSnapshot,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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

pub fn render_node_scratch(session: &SessionSnapshot, node: &ProofNode) -> String {
    let content = clean_lean_content(node.content.trim());
    let content = content.trim();

    // If the content already has import statements, it's a self-contained Lean file.
    // Use it as-is to avoid duplicate imports.
    if content.starts_with("import ") {
        return content.to_string();
    }

    let imports = if session.proof.imports.is_empty() {
        vec!["Mathlib".to_string()]
    } else {
        dedup_strings(session.proof.imports.clone())
    };
    let mut lines = Vec::new();
    for import in imports {
        lines.push(format!("import {import}"));
    }
    lines.push(String::new());
    lines.push(format!("-- openproof: {} :: {}", escape_comment(&node.label), escape_comment(&node.statement)));
    lines.push(String::new());
    lines.push(content.to_string());
    lines.join("\n")
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
        dedup_strings(imports.to_vec())
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

fn verify_scratch(
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

    let output = Command::new("lake")
        .arg("env")
        .arg("lean")
        .arg(&scratch_path)
        .current_dir(project_dir)
        .output()
        .with_context(|| format!("running lake env lean {}", scratch_path.display()))?;

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

fn write_temp_scratch(rendered_scratch: &str) -> Result<PathBuf> {
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

// --- Agent tools: structured Lean interaction ---

/// Extract the goal states at each `sorry` in a Lean file.
/// Returns a list of (line_number, goal_description) pairs.
pub fn extract_sorry_goals(project_dir: &Path, content: &str) -> Result<Vec<(usize, String)>> {
    let scratch_path = write_temp_scratch(content)?;
    let output = Command::new("lake")
        .arg("env")
        .arg("lean")
        .arg(&scratch_path)
        .current_dir(project_dir)
        .output()
        .context("running lean for goal extraction")?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stderr}\n{stdout}");

    let mut goals = Vec::new();
    let mut current_line: Option<usize> = None;
    let mut current_goal = String::new();
    let mut in_goal = false;

    for line in combined.lines() {
        // Match patterns like "Scratch.lean:15:4: error: unsolved goals"
        // or "declaration uses 'sorry'"
        if line.contains("unsolved goals") || line.contains("uses 'sorry'") {
            // Extract line number
            if let Some(ln) = extract_line_number(line, &scratch_path) {
                if in_goal && current_line.is_some() {
                    goals.push((current_line.unwrap(), current_goal.trim().to_string()));
                }
                current_line = Some(ln);
                current_goal.clear();
                in_goal = true;
            }
        } else if in_goal {
            // Goal state lines are indented or follow the error
            let trimmed = line.trim();
            if trimmed.is_empty() && !current_goal.is_empty() {
                // End of this goal block
                if let Some(ln) = current_line {
                    goals.push((ln, current_goal.trim().to_string()));
                }
                current_goal.clear();
                current_line = None;
                in_goal = false;
            } else if !trimmed.is_empty() {
                current_goal.push_str(trimmed);
                current_goal.push('\n');
            }
        }
    }
    // Flush last goal
    if in_goal && current_line.is_some() && !current_goal.trim().is_empty() {
        goals.push((current_line.unwrap(), current_goal.trim().to_string()));
    }

    Ok(goals)
}

/// Run `exact?` at the first `sorry` in the content and return Lean's suggestions.
/// Returns a list of suggested tactics (e.g., "exact Nat.Prime.dvd_factorial").
pub fn run_tactic_suggestions(
    project_dir: &Path,
    content: &str,
    tactic: &str, // "exact?" or "apply?" or "rw?"
) -> Result<Vec<String>> {
    // Replace the first `sorry` with the search tactic
    let modified = if let Some(pos) = content.find("sorry") {
        format!("{}{}{}",
            &content[..pos],
            tactic,
            &content[pos + "sorry".len()..])
    } else {
        // No sorry found -- append the tactic as a standalone check
        format!("{content}\n#check {tactic}")
    };

    let scratch_path = write_temp_scratch(&modified)?;
    let output = Command::new("lake")
        .arg("env")
        .arg("lean")
        .arg(&scratch_path)
        .current_dir(project_dir)
        .output()
        .context("running lean for tactic suggestions")?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stderr}\n{stdout}");

    let mut suggestions = Vec::new();
    for line in combined.lines() {
        let trimmed = line.trim();
        // Parse "Try this: exact foo" or "[apply] exact foo"
        if let Some(rest) = trimmed.strip_prefix("Try this:") {
            suggestions.push(rest.trim().to_string());
        } else if trimmed.starts_with("[exact]") || trimmed.starts_with("[apply]") || trimmed.starts_with("[rw]") {
            // Format: "[exact] exact Some.Lemma"
            if let Some(pos) = trimmed.find(']') {
                suggestions.push(trimmed[pos + 1..].trim().to_string());
            }
        }
    }

    Ok(suggestions)
}

fn extract_line_number(error_line: &str, scratch_path: &Path) -> Option<usize> {
    // Pattern: "/path/Scratch.lean:15:4: error: ..."
    let filename = scratch_path.file_name()?.to_str()?;
    let parts: Vec<&str> = error_line.split(':').collect();
    for (i, part) in parts.iter().enumerate() {
        if part.contains(filename) || part.ends_with(".lean") {
            if let Some(line_str) = parts.get(i + 1) {
                return line_str.trim().parse().ok();
            }
        }
    }
    None
}

/// Extract grounding facts from Lean output: #check results, type signatures,
/// "Try this:" suggestions, and known-good lemma names that Lean reports.
/// These should be presented front-and-center to the repairer, not buried in error noise.
pub fn extract_grounding_from_lean_output(stderr: &str, stdout: &str) -> Vec<String> {
    let combined = format!("{stderr}\n{stdout}");
    let mut facts = Vec::new();

    for line in combined.lines() {
        let trimmed = line.trim();

        // "Try this: exact Nat.bertrand hn0"
        if let Some(rest) = trimmed.strip_prefix("Try this:") {
            facts.push(format!("LEAN SUGGESTS: {}", rest.trim()));
        }
        // "[apply] exact ZMod.pow_card_sub_one_eq_one hxi"
        if trimmed.starts_with("[exact]") || trimmed.starts_with("[apply]") || trimmed.starts_with("[rw]") {
            if let Some(pos) = trimmed.find(']') {
                facts.push(format!("LEAN SUGGESTS: {}", trimmed[pos + 1..].trim()));
            }
        }
        // Type signatures from #check: "Nat.bertrand (n : ℕ) (hn0 : n ≠ 0) : ∃ p, ..."
        if trimmed.contains(" : ") && !trimmed.contains("error") && !trimmed.starts_with('/') {
            // Heuristic: lines containing " : " that aren't errors are likely type signatures
            let has_known_pattern = trimmed.contains("→") || trimmed.contains("∀")
                || trimmed.contains("∃") || trimmed.contains("Prop")
                || (trimmed.contains("(") && trimmed.contains(":"));
            if has_known_pattern && trimmed.len() > 10 && trimmed.len() < 500 {
                facts.push(format!("LEAN REPORTS: {trimmed}"));
            }
        }
    }

    facts.dedup();
    facts
}

/// Strip openproof structured markers that may have leaked into Lean code.
/// These are text markers like "LEMMA: label :: statement" that the model
/// sometimes includes in its lean code blocks.
/// A declaration extracted from a Lean source file.
#[derive(Debug, Clone)]
pub struct LeanDeclaration {
    pub kind: &'static str, // "theorem", "lemma", "def", "axiom"
    pub name: String,
    pub signature: String, // everything after the name up to := or where
    pub body: String,      // the full declaration text
    pub line: usize,
}

/// Parse a Lean source file and extract all top-level declarations.
/// Returns declarations in source order.
pub fn parse_lean_declarations(content: &str) -> Vec<LeanDeclaration> {
    let mut decls = Vec::new();
    let keywords = ["theorem", "lemma", "def", "noncomputable def", "axiom"];
    let lines: Vec<&str> = content.lines().collect();

    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();

        // Check if this line starts a declaration
        let matched_kw = keywords.iter().find(|&&kw| {
            trimmed.starts_with(kw) && trimmed[kw.len()..].starts_with(|c: char| c.is_whitespace())
        });

        if let Some(&kw) = matched_kw {
            let canonical_kind = match kw {
                "noncomputable def" => "def",
                other => other,
            };

            // Extract the declaration name
            let after_kw = trimmed[kw.len()..].trim();
            let name = after_kw
                .split(|c: char| c.is_whitespace() || c == '(' || c == ':' || c == '{' || c == '[')
                .next()
                .unwrap_or("")
                .to_string();

            if name.is_empty() || name.starts_with('-') {
                i += 1;
                continue;
            }

            // Extract the type signature (everything between name and := / where / by)
            let decl_start = i;
            let mut signature = String::new();
            let mut body_lines = vec![lines[i].to_string()];

            // Collect the full declaration body (until next top-level declaration or blank line after content)
            let mut j = i + 1;
            let mut found_body = trimmed.contains(":=") || trimmed.contains(" by") || trimmed.contains(" where");
            while j < lines.len() {
                let next = lines[j];
                let next_trimmed = next.trim();

                // Stop at next top-level declaration
                if !next_trimmed.is_empty()
                    && !next.starts_with(' ')
                    && !next.starts_with('\t')
                    && keywords.iter().any(|&kw| {
                        next_trimmed.starts_with(kw) && next_trimmed[kw.len()..].starts_with(|c: char| c.is_whitespace())
                    })
                {
                    break;
                }

                // Stop at section/namespace/end markers
                if next_trimmed.starts_with("section")
                    || next_trimmed.starts_with("namespace")
                    || next_trimmed == "end"
                    || next_trimmed.starts_with("end ")
                    || next_trimmed.starts_with("#")
                {
                    break;
                }

                body_lines.push(next.to_string());
                if next_trimmed.contains(":=") || next_trimmed.contains(" by") || next_trimmed.contains(" where") {
                    found_body = true;
                }
                j += 1;
            }

            // Extract signature: text between name and := / by
            let full_text = body_lines.join("\n");
            if let Some(name_pos) = full_text.find(&name) {
                let after_name = &full_text[name_pos + name.len()..];
                let sig_end = after_name
                    .find(":=")
                    .or_else(|| after_name.find(" by\n"))
                    .or_else(|| after_name.find(" by "))
                    .or_else(|| after_name.find(" where"))
                    .unwrap_or(after_name.len());
                signature = after_name[..sig_end].trim().to_string();
            }

            decls.push(LeanDeclaration {
                kind: canonical_kind,
                name,
                signature,
                body: full_text,
                line: decl_start + 1,
            });

            i = j;
        } else {
            i += 1;
        }
    }

    decls
}

/// Convert parsed Lean declarations into ProofNode entries for the proof tree.
/// The first theorem/lemma becomes the root; subsequent ones are children.
pub fn declarations_to_proof_nodes(
    decls: &[LeanDeclaration],
    session_id: &str,
) -> Vec<ProofNode> {
    let now = Utc::now().to_rfc3339();
    let mut nodes = Vec::new();
    let mut root_id: Option<String> = None;

    for (i, decl) in decls.iter().enumerate() {
        let kind = match decl.kind {
            "theorem" => ProofNodeKind::Theorem,
            "lemma" => ProofNodeKind::Lemma,
            _ => ProofNodeKind::Artifact,
        };

        let id = format!("lean_{session_id}_{}", decl.name);
        let is_root = i == 0 && matches!(kind, ProofNodeKind::Theorem);

        if is_root {
            root_id = Some(id.clone());
        }

        let parent_id = if is_root {
            None
        } else {
            root_id.clone()
        };

        let depth = if is_root { 0 } else { 1 };

        nodes.push(ProofNode {
            id,
            kind,
            label: decl.name.clone(),
            statement: decl.signature.clone(),
            content: decl.body.clone(),
            status: ProofNodeStatus::Pending,
            parent_id,
            depends_on: Vec::new(),
            depth,
            created_at: now.clone(),
            updated_at: now.clone(),
        });
    }

    nodes
}

fn clean_lean_content(content: &str) -> String {
    content
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            // Filter out marker lines that aren't valid Lean
            !trimmed.starts_with("LEMMA:")
                && !trimmed.starts_with("THEOREM:")
                && !trimmed.starts_with("TITLE:")
                && !trimmed.starts_with("PROBLEM:")
                && !trimmed.starts_with("STATUS:")
                && !trimmed.starts_with("PHASE:")
                && !trimmed.starts_with("NEXT:")
                && !trimmed.starts_with("PAPER:")
                && !trimmed.starts_with("FORMAL_TARGET:")
                && !trimmed.starts_with("ACCEPTED_TARGET:")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn dedup_strings(values: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut result = Vec::new();
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed.to_string()) {
            result.push(trimmed.to_string());
        }
    }
    result
}

fn escape_comment(input: &str) -> String {
    input.replace("*/", "* /").replace('\n', " ")
}

fn contains_sorry_placeholder(stdout: &str, stderr: &str) -> bool {
    let combined = format!("{stdout}\n{stderr}").to_ascii_lowercase();
    combined.contains("warning: declaration uses 'sorry'")
        || combined.contains("uses 'sorry'")
        || combined.contains("uses sorry")
        || combined.contains("declaration has sorry")
}
