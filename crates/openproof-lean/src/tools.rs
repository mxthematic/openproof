//! Tool execution for the LLM agent's coding tools.
//!
//! Each tool operates within a sandboxed session workspace directory.
//! Paths are validated to prevent escaping the workspace.

use anyhow::{Context, Result};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::lsp_mcp::LeanLspMcp;

/// Maximum output size returned to the model (characters).
const MAX_OUTPUT_CHARS: usize = 8000;

/// Timeout for Lean commands (generous -- large Mathlib imports are slow).
const LEAN_TIMEOUT_SECS: u64 = 600;

/// Context needed to execute tools.
pub struct ToolContext<'a> {
    /// Path to the Lean project (contains lakefile.toml).
    pub project_dir: &'a Path,
    /// Path to the session workspace directory.
    pub workspace_dir: &'a Path,
    /// Current import list for the session.
    pub imports: &'a [String],
    /// Optional lean-lsp-mcp client for structured goal access.
    pub lsp_mcp: Option<Arc<Mutex<LeanLspMcp>>>,
    /// Optional shared Pantograph + proof tree state for fast tactic testing.
    pub prover: Option<crate::proof_tree::SharedProver>,
}

/// Result of executing a tool.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub success: bool,
    pub content: String,
}

/// Execute a tool by name with JSON arguments.
pub fn execute_tool(name: &str, arguments: &str, ctx: &ToolContext) -> ToolOutput {
    let args: Value = serde_json::from_str(arguments).unwrap_or(Value::Object(Default::default()));
    let result = match name {
        "lean_verify" => tool_lean_verify(&args, ctx),
        "lean_goals" => tool_lean_goals(&args, ctx),
        "lean_screen_tactics" => tool_lean_screen_tactics(&args, ctx),
        "lean_check" => tool_lean_check(&args, ctx),
        "lean_eval" => tool_lean_eval_fn(&args, ctx),
        "lean_search_tactic" => tool_lean_search_tactic(&args, ctx),
        "file_read" => tool_file_read(&args, ctx),
        "file_write" => tool_file_write(&args, ctx),
        "file_patch" => tool_file_patch(&args, ctx),
        "workspace_ls" => tool_workspace_ls(ctx),
        "shell_run" => tool_shell_run(&args, ctx),
        _ => Err(anyhow::anyhow!("unknown tool: {name}")),
    };
    match result {
        Ok(output) => output,
        Err(err) => ToolOutput {
            success: false,
            content: truncate_output(&format!("Error: {err:#}")),
        },
    }
}

fn tool_lean_verify(args: &Value, ctx: &ToolContext) -> Result<ToolOutput> {
    let file = args
        .get("file")
        .and_then(Value::as_str)
        .unwrap_or("Scratch.lean");
    let target = sanitize_path(ctx.workspace_dir, file)?;
    let content = fs::read_to_string(&target)
        .with_context(|| format!("reading {file}"))?;

    let full_content = build_compilation_unit(&content, ctx);

    let scratch_path = write_temp_file(&full_content)?;
    let (ok, output) = run_lean_command(ctx.project_dir, &scratch_path)?;
    let has_sorry = output.contains("declaration uses 'sorry'");

    Ok(ToolOutput {
        success: ok && !has_sorry,
        content: truncate_output(&output),
    })
}

/// Build a complete compilation unit from file content.
/// Includes imports, corpus declarations (from CorpusHits.lean), and the file content.
/// This makes corpus-retrieved declarations available via `exact <name>`.
fn build_compilation_unit(content: &str, ctx: &ToolContext) -> String {
    // Split content into imports and body
    let (user_imports, body) = if content.trim_start().starts_with("import ") {
        let mut imports = Vec::new();
        let mut body_start = 0;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("import ") || trimmed.starts_with("open ") || trimmed.is_empty() {
                imports.push(line.to_string());
                body_start += line.len() + 1; // +1 for newline
            } else {
                break;
            }
        }
        let body = if body_start < content.len() {
            &content[body_start..]
        } else {
            ""
        };
        (imports, body.to_string())
    } else {
        let imports: Vec<String> = if ctx.imports.is_empty() {
            vec!["import Mathlib".to_string()]
        } else {
            ctx.imports.iter().map(|i| format!("import {i}")).collect()
        };
        (imports, content.to_string())
    };

    // Strip any user-written `import OpenProof.Corpus` -- we manage this automatically.
    let mut lines: Vec<String> = user_imports
        .into_iter()
        .filter(|l| !l.contains("OpenProof.Corpus"))
        .collect();

    // Only add the corpus import if the compiled olean actually exists.
    if crate::corpus_module::corpus_olean_exists(ctx.project_dir) {
        lines.push("import OpenProof.Corpus".to_string());
    }

    lines.push(String::new());

    // Also strip corpus import from the body (model may have written it inline)
    let body = body
        .lines()
        .filter(|l| !l.trim().starts_with("import OpenProof.Corpus"))
        .collect::<Vec<_>>()
        .join("\n");

    lines.push(body);
    lines.join("\n")
}

fn tool_lean_goals(args: &Value, ctx: &ToolContext) -> Result<ToolOutput> {
    let file = args
        .get("file")
        .and_then(Value::as_str)
        .unwrap_or("Scratch.lean");
    let target = sanitize_path(ctx.workspace_dir, file)?;

    // Try MCP for structured goals.
    // MCP needs files in the Lean project dir, so copy workspace file there.
    if let Some(ref lsp) = ctx.lsp_mcp {
        if let Ok(mut mcp) = lsp.lock() {
            if mcp.is_alive() {
                let content = fs::read_to_string(&target)
                    .with_context(|| format!("reading {file}"))?;
                // Write to project dir so MCP can find it
                let project_scratch = ctx.project_dir.join("Scratch.lean");
                let _ = fs::write(&project_scratch, &content);
                let sorry_positions = find_sorry_positions(&content);

                if sorry_positions.is_empty() {
                    return Ok(ToolOutput {
                        success: true,
                        content: "No sorry positions found in the file.".to_string(),
                    });
                }

                let mut output_parts = Vec::new();
                for (line, _col) in &sorry_positions {
                    match mcp.get_goals(&project_scratch, *line, None) {
                        Ok(goal_state) => {
                            let goals = goal_state.goals_before.as_ref()
                                .or(goal_state.goals.as_ref())
                                .map(|g| g.join("\n---\n"))
                                .unwrap_or_else(|| "(no goals)".to_string());
                            output_parts.push(format!(
                                "Line {line}:\n{goals}"
                            ));
                        }
                        Err(e) => {
                            output_parts.push(format!("Line {line}: error: {e}"));
                        }
                    }
                }

                return Ok(ToolOutput {
                    success: true,
                    content: truncate_output(&output_parts.join("\n\n")),
                });
            }
        }
    }

    // Fallback: use regex-based goal extraction
    let content = fs::read_to_string(&target)
        .with_context(|| format!("reading {file}"))?;

    let full_content = if content.trim_start().starts_with("import ") {
        content
    } else {
        let imports = build_import_block(ctx.imports);
        format!("{imports}\n{content}")
    };

    match crate::goals::extract_sorry_goals(ctx.project_dir, &full_content) {
        Ok(goals) if goals.is_empty() => Ok(ToolOutput {
            success: true,
            content: "No sorry goals found.".to_string(),
        }),
        Ok(goals) => {
            let formatted: Vec<String> = goals
                .iter()
                .map(|(line, goal)| format!("Line {line}:\n{goal}"))
                .collect();
            Ok(ToolOutput {
                success: true,
                content: truncate_output(&formatted.join("\n\n")),
            })
        }
        Err(e) => Ok(ToolOutput {
            success: false,
            content: truncate_output(&format!("Goal extraction failed: {e}")),
        }),
    }
}

fn tool_lean_screen_tactics(args: &Value, ctx: &ToolContext) -> Result<ToolOutput> {
    let file = args
        .get("file")
        .and_then(Value::as_str)
        .unwrap_or("Scratch.lean");
    let line = args
        .get("line")
        .and_then(Value::as_u64)
        .context("missing 'line' argument")? as usize;
    let tactics: Vec<String> = args
        .get("tactics")
        .and_then(Value::as_array)
        .context("missing 'tactics' argument")?
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();

    if tactics.is_empty() {
        return Ok(ToolOutput {
            success: false,
            content: "No tactics provided.".to_string(),
        });
    }

    let target = sanitize_path(ctx.workspace_dir, file)?;

    // Try Pantograph first (fastest: ~100ms per tactic compile vs 30s)
    if let Some(ref prover) = ctx.prover {
        if let Ok(mut sp) = prover.lock() {
            if sp.is_alive() {
                let content = fs::read_to_string(&target)
                    .with_context(|| format!("reading {file}"))?;
                // Strip imports -- Pantograph has Mathlib preloaded
                let no_imports: String = content.lines()
                    .filter(|l| !l.trim().starts_with("import ") && !l.trim().starts_with("open "))
                    .collect::<Vec<_>>()
                    .join("\n");

                let goal_hash = crate::proof_tree::hash_goal(&format!("{file}:{line}"));
                let mut parts = Vec::new();
                for tactic in &tactics {
                    // Skip tactics known to fail for this goal
                    if sp.tree.is_known_failure(goal_hash, tactic) {
                        parts.push(format!("[SKIPPED] {tactic} (known failure)"));
                        continue;
                    }
                    sp.tree.record_attempt();

                    let modified = replace_sorry_at_line(&no_imports, line, tactic);
                    match sp.pantograph.verify_content(&modified) {
                        Ok(result) => {
                            let status = if result.ok {
                                "SOLVED"
                            } else if result.has_sorry {
                                "ok"
                            } else {
                                sp.tree.record_failure(goal_hash, tactic);
                                "FAILED"
                            };
                            let mut entry = format!("[{status}] {tactic}");
                            if status == "FAILED" {
                                if let Some(err) = result.messages.iter().find(|m| m.contains("error")) {
                                    entry.push_str(&format!("\n  Error: {}", err.lines().next().unwrap_or("")));
                                }
                            }
                            parts.push(entry);
                        }
                        Err(_) => {
                            sp.tree.record_failure(goal_hash, tactic);
                            parts.push(format!("[FAILED] {tactic}\n  Error: Pantograph error"));
                        }
                    }
                }
                return Ok(ToolOutput {
                    success: true,
                    content: truncate_output(&parts.join("\n")),
                });
            }
        }
    }

    // Try MCP for multi-attempt screening (fallback)
    if let Some(ref lsp) = ctx.lsp_mcp {
        if let Ok(mut mcp) = lsp.lock() {
            if mcp.is_alive() {
                match mcp.screen_tactics(&target, line, None, &tactics) {
                    Ok(result) => {
                        let mut parts = Vec::new();
                        for item in &result.items {
                            let status = if item.is_solved() {
                                "SOLVED"
                            } else if item.succeeded() {
                                "ok"
                            } else {
                                "FAILED"
                            };
                            let mut entry = format!("[{status}] {}", item.snippet);
                            if !item.goals.is_empty() {
                                entry.push_str(&format!(
                                    "\n  Goals: {}",
                                    item.goals.join(" | ")
                                ));
                            }
                            for diag in &item.diagnostics {
                                if diag.severity == "error" {
                                    entry.push_str(&format!(
                                        "\n  Error: {}",
                                        diag.message.lines().next().unwrap_or("")
                                    ));
                                }
                            }
                            parts.push(entry);
                        }
                        return Ok(ToolOutput {
                            success: true,
                            content: truncate_output(&parts.join("\n")),
                        });
                    }
                    Err(_e) => {
                        // Fall through to fallback
                    }
                }
            }
        }
    }

    // Fallback: try each tactic via lean_search_tactic one at a time
    let content = fs::read_to_string(&target)
        .with_context(|| format!("reading {file}"))?;

    let full_content = if content.trim_start().starts_with("import ") {
        content
    } else {
        let imports = build_import_block(ctx.imports);
        format!("{imports}\n{content}")
    };

    let mut parts = Vec::new();
    for tactic in &tactics {
        let modified = replace_sorry_at_line(&full_content, line, tactic);
        let scratch_path = write_temp_file(&modified)?;
        let (ok, output) = run_lean_command(ctx.project_dir, &scratch_path)?;
        let status = if ok && !output.contains("sorry") {
            "SOLVED"
        } else if ok {
            "ok"
        } else {
            "FAILED"
        };
        let first_error = output.lines()
            .find(|l| l.contains("error"))
            .unwrap_or("")
            .trim();
        let mut entry = format!("[{status}] {tactic}");
        if !first_error.is_empty() && status == "FAILED" {
            entry.push_str(&format!("\n  Error: {first_error}"));
        }
        parts.push(entry);
    }

    Ok(ToolOutput {
        success: true,
        content: truncate_output(&parts.join("\n")),
    })
}

/// Find (line, column) positions of all `sorry` tokens in content.
pub fn find_sorry_positions(content: &str) -> Vec<(usize, usize)> {
    let mut positions = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let mut start = 0;
        while let Some(pos) = line[start..].find("sorry") {
            let abs_pos = start + pos;
            // Check it's a word boundary (not part of a larger identifier)
            let before_ok = abs_pos == 0
                || !line.as_bytes()[abs_pos - 1].is_ascii_alphanumeric();
            let after_pos = abs_pos + 5;
            let after_ok = after_pos >= line.len()
                || !line.as_bytes()[after_pos].is_ascii_alphanumeric();
            if before_ok && after_ok {
                positions.push((i + 1, abs_pos + 1)); // 1-indexed
            }
            start = abs_pos + 5;
        }
    }
    positions
}

fn tool_lean_check(args: &Value, ctx: &ToolContext) -> Result<ToolOutput> {
    let expr = args
        .get("expr")
        .and_then(Value::as_str)
        .context("missing 'expr' argument")?;
    let imports = build_import_block(ctx.imports);
    let content = format!("{imports}\n#check {expr}\n");
    let scratch_path = write_temp_file(&content)?;
    let (ok, output) = run_lean_command(ctx.project_dir, &scratch_path)?;
    Ok(ToolOutput {
        success: ok,
        content: truncate_output(&output),
    })
}

fn tool_lean_eval_fn(args: &Value, ctx: &ToolContext) -> Result<ToolOutput> {
    let expr = args
        .get("expr")
        .and_then(Value::as_str)
        .context("missing 'expr' argument")?;
    let imports = build_import_block(ctx.imports);
    // #eval runs a Lean expression and prints the result
    let content = format!("{imports}\n#eval ({expr})\n");
    let scratch_path = write_temp_file(&content)?;
    let (ok, output) = run_lean_command(ctx.project_dir, &scratch_path)?;
    Ok(ToolOutput {
        success: ok,
        content: truncate_output(&output),
    })
}

fn tool_lean_search_tactic(args: &Value, ctx: &ToolContext) -> Result<ToolOutput> {
    let tactic = args
        .get("tactic")
        .and_then(Value::as_str)
        .context("missing 'tactic' argument")?;
    let file = args
        .get("file")
        .and_then(Value::as_str)
        .unwrap_or("Scratch.lean");
    let target_line = args.get("line").and_then(Value::as_u64);

    let target = sanitize_path(ctx.workspace_dir, file)?;
    let content = fs::read_to_string(&target)
        .with_context(|| format!("reading {file}"))?;

    // Replace the sorry at the specified line (or first sorry) with the search tactic.
    let modified = if let Some(line_num) = target_line {
        replace_sorry_at_line(&content, line_num as usize, tactic)
    } else {
        replace_first_sorry(&content, tactic)
    };

    // Prepend imports if needed.
    let full_content = if modified.trim_start().starts_with("import ") {
        modified
    } else {
        let imports = build_import_block(ctx.imports);
        format!("{imports}\n{modified}")
    };

    let scratch_path = write_temp_file(&full_content)?;
    let (_ok, output) = run_lean_command(ctx.project_dir, &scratch_path)?;

    // Extract just the suggestions from the output.
    let mut suggestions = Vec::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Try this:") {
            suggestions.push(rest.trim().to_string());
        } else if trimmed.starts_with("[exact]")
            || trimmed.starts_with("[apply]")
            || trimmed.starts_with("[rw]")
        {
            if let Some(pos) = trimmed.find(']') {
                suggestions.push(trimmed[pos + 1..].trim().to_string());
            }
        }
    }

    if suggestions.is_empty() {
        Ok(ToolOutput {
            success: false,
            content: truncate_output(&format!("No suggestions found.\n\nFull output:\n{output}")),
        })
    } else {
        Ok(ToolOutput {
            success: true,
            content: truncate_output(&suggestions.join("\n")),
        })
    }
}

fn tool_file_read(args: &Value, ctx: &ToolContext) -> Result<ToolOutput> {
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .context("missing 'path' argument")?;
    let target = sanitize_path(ctx.workspace_dir, path)?;
    let content = fs::read_to_string(&target)
        .with_context(|| format!("reading {path}"))?;
    // Add line numbers.
    let numbered: String = content
        .lines()
        .enumerate()
        .map(|(i, line)| format!("{:4}  {line}", i + 1))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(ToolOutput {
        success: true,
        content: truncate_output(&numbered),
    })
}

fn tool_file_write(args: &Value, ctx: &ToolContext) -> Result<ToolOutput> {
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .context("missing 'path' argument")?;
    let content = args
        .get("content")
        .and_then(Value::as_str)
        .context("missing 'content' argument")?;
    let target = sanitize_path(ctx.workspace_dir, path)?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&target, content)
        .with_context(|| format!("writing {path}"))?;
    let size = content.len();
    Ok(ToolOutput {
        success: true,
        content: format!("Wrote {size} bytes to {path}"),
    })
}

fn tool_file_patch(args: &Value, ctx: &ToolContext) -> Result<ToolOutput> {
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .context("missing 'path' argument")?;
    let patch_text = args
        .get("patch")
        .and_then(Value::as_str)
        .context("missing 'patch' argument")?;
    let target = sanitize_path(ctx.workspace_dir, path)?;
    let original = fs::read_to_string(&target)
        .with_context(|| format!("reading {path} for patching"))?;

    match crate::patch::apply_patch(&original, patch_text) {
        Some(result) => {
            fs::write(&target, &result.patched_content)
                .with_context(|| format!("writing patched {path}"))?;
            // Include the actual diff hunks so the TUI can render colored diffs
            let mut output = format!(
                "Patch applied to {path}: {} hunks, +{} -{} lines",
                result.hunks_applied, result.lines_added, result.lines_removed
            );
            // Extract diff lines from the patch text for display
            for line in patch_text.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with('+') || trimmed.starts_with('-') || trimmed.starts_with("@@") {
                    output.push('\n');
                    output.push_str(trimmed);
                }
            }
            Ok(ToolOutput {
                success: true,
                content: output,
            })
        }
        None => Ok(ToolOutput {
            success: false,
            content: "Patch failed: could not match context lines in the file".to_string(),
        }),
    }
}

fn tool_workspace_ls(ctx: &ToolContext) -> Result<ToolOutput> {
    if !ctx.workspace_dir.exists() {
        return Ok(ToolOutput {
            success: true,
            content: "(empty workspace)".to_string(),
        });
    }
    let mut entries = Vec::new();
    walk_dir(ctx.workspace_dir, ctx.workspace_dir, &mut entries)?;
    entries.sort();
    if entries.is_empty() {
        Ok(ToolOutput {
            success: true,
            content: "(empty workspace)".to_string(),
        })
    } else {
        Ok(ToolOutput {
            success: true,
            content: entries.join("\n"),
        })
    }
}

// --- Helpers ---

fn sanitize_path(workspace_dir: &Path, relative: &str) -> Result<PathBuf> {
    let rel = Path::new(relative);
    anyhow::ensure!(rel.is_relative(), "path must be relative: {relative}");
    for component in rel.components() {
        if matches!(component, std::path::Component::ParentDir) {
            anyhow::bail!("path must not contain '..': {relative}");
        }
    }
    Ok(workspace_dir.join(rel))
}

fn write_temp_file(content: &str) -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!(
        "openproof-lean-{}",
        std::process::id()
    ));
    fs::create_dir_all(&dir)?;
    let path = dir.join("Scratch.lean");
    fs::write(&path, content)?;
    Ok(path)
}

/// Compile Lean content and return (ok, raw_output).
/// Public so the tactic search can extract goal types from error output.
pub fn run_lean_verify_raw(project_dir: &Path, content: &str) -> Result<(bool, String)> {
    let full = if content.trim_start().starts_with("import ") {
        content.to_string()
    } else {
        format!("import Mathlib\n\n{content}")
    };
    let scratch_path = write_temp_file(&full)?;
    run_lean_command(project_dir, &scratch_path)
}

fn build_import_block(imports: &[String]) -> String {
    let list = if imports.is_empty() {
        vec!["Mathlib".to_string()]
    } else {
        imports.to_vec()
    };
    list.iter()
        .map(|i| format!("import {i}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Cached LEAN_PATH resolved from `lake env` on first use.
/// Avoids ~2.5s overhead per verification call.
static CACHED_LEAN_PATH: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

pub fn resolve_lean_path(project_dir: &Path) -> Option<String> {
    CACHED_LEAN_PATH
        .get_or_init(|| {
            Command::new("lake")
                .arg("env")
                .arg("sh")
                .arg("-c")
                .arg("echo $LEAN_PATH")
                .current_dir(project_dir)
                .output()
                .ok()
                .and_then(|out| {
                    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    if path.is_empty() { None } else { Some(path) }
                })
        })
        .clone()
}

fn run_lean_command(project_dir: &Path, scratch_path: &Path) -> Result<(bool, String)> {
    // Use cached LEAN_PATH to call lean directly (saves ~2.5s lake overhead).
    // Falls back to `lake env lean` if path resolution fails.
    let child = if let Some(lean_path) = resolve_lean_path(project_dir) {
        Command::new("lean")
            .arg("--threads=4")
            .arg(scratch_path)
            .env("LEAN_PATH", &lean_path)
            .current_dir(project_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .context("spawning lean with cached LEAN_PATH")?
    } else {
        Command::new("lake")
            .arg("env")
            .arg("lean")
            .arg(scratch_path)
            .current_dir(project_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .context("spawning lake env lean")?
    };

    let output = wait_with_timeout(child, Duration::from_secs(LEAN_TIMEOUT_SECS))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stderr}{stdout}").trim().to_string();
    let ok = output.status.success();
    Ok((ok, combined))
}

fn wait_with_timeout(
    mut child: std::process::Child,
    timeout: Duration,
) -> Result<std::process::Output> {
    let start = std::time::Instant::now();
    loop {
        match child.try_wait()? {
            Some(status) => {
                let stdout = child.stdout.map(|mut s| {
                    let mut buf = Vec::new();
                    std::io::Read::read_to_end(&mut s, &mut buf).ok();
                    buf
                }).unwrap_or_default();
                let stderr = child.stderr.map(|mut s| {
                    let mut buf = Vec::new();
                    std::io::Read::read_to_end(&mut s, &mut buf).ok();
                    buf
                }).unwrap_or_default();
                return Ok(std::process::Output { status, stdout, stderr });
            }
            None => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    anyhow::bail!("Lean command timed out after {}s", timeout.as_secs());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn replace_first_sorry(content: &str, tactic: &str) -> String {
    if let Some(pos) = content.find("sorry") {
        format!(
            "{}{}{}",
            &content[..pos],
            tactic,
            &content[pos + "sorry".len()..]
        )
    } else {
        content.to_string()
    }
}

fn replace_sorry_at_line(content: &str, target_line: usize, tactic: &str) -> String {
    let mut result = String::new();
    let mut replaced = false;
    for (i, line) in content.lines().enumerate() {
        if i + 1 == target_line && !replaced {
            if let Some(pos) = line.find("sorry") {
                result.push_str(&line[..pos]);
                result.push_str(tactic);
                result.push_str(&line[pos + "sorry".len()..]);
                result.push('\n');
                replaced = true;
                continue;
            }
        }
        result.push_str(line);
        result.push('\n');
    }
    if !replaced {
        return replace_first_sorry(content, tactic);
    }
    result
}

fn truncate_output(s: &str) -> String {
    if s.len() <= MAX_OUTPUT_CHARS {
        s.to_string()
    } else {
        let truncated = &s[..MAX_OUTPUT_CHARS];
        format!("{truncated}\n\n... (output truncated at {MAX_OUTPUT_CHARS} characters)")
    }
}

fn walk_dir(base: &Path, current: &Path, out: &mut Vec<String>) -> Result<()> {
    let entries = fs::read_dir(current)?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.file_name().map(|n| n == "history").unwrap_or(false) && path.is_dir() {
            continue;
        }
        if path.is_dir() {
            walk_dir(base, &path, out)?;
        } else {
            let relative = path.strip_prefix(base).unwrap_or(&path);
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            out.push(format!("{:<40} {:>8} bytes", relative.display(), size));
        }
    }
    Ok(())
}

fn tool_shell_run(args: &Value, ctx: &ToolContext) -> Result<ToolOutput> {
    let command = args
        .get("command")
        .and_then(Value::as_str)
        .context("missing 'command' argument")?;

    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(ctx.workspace_dir)
        .output()
        .context("failed to run shell command")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = if stderr.is_empty() {
        stdout.to_string()
    } else {
        format!("{stdout}\n--- stderr ---\n{stderr}")
    };

    Ok(ToolOutput {
        success: output.status.success(),
        content: truncate_output(&combined),
    })
}

