//! Patch parser and applier for surgical Lean file edits.
//!
//! Supports the aegis-style patch format:
//! ```text
//! *** Begin Patch
//! *** Update File: Scratch.lean
//! @@ context
//!  context line
//! -old line
//! +new line
//!  context line
//! *** End Patch
//! ```

/// Result of applying a patch.
#[derive(Debug, Clone)]
pub struct PatchResult {
    pub patched_content: String,
    pub diff_summary: String,
    pub lines_added: usize,
    pub lines_removed: usize,
    pub hunks_applied: usize,
}

/// A single hunk in a patch.
#[derive(Debug, Clone)]
struct Hunk {
    context_before: Vec<String>,
    removals: Vec<String>,
    additions: Vec<String>,
}

/// Extract a patch block from model output text.
/// Returns None if no patch format is found.
pub fn extract_patch(text: &str) -> Option<String> {
    let start = text.find("*** Begin Patch")?;
    let end = text.find("*** End Patch")?;
    if end <= start {
        return None;
    }
    Some(text[start..end + "*** End Patch".len()].to_string())
}

/// Check if text contains a patch (quick check).
pub fn contains_patch(text: &str) -> bool {
    text.contains("*** Begin Patch") && text.contains("*** End Patch")
}

/// Parse and apply a patch to the given file content.
/// Returns the patched content and a summary, or None if the patch can't be applied.
pub fn apply_patch(original: &str, patch_text: &str) -> Option<PatchResult> {
    let hunks = parse_hunks(patch_text)?;
    if hunks.is_empty() {
        return None;
    }

    let mut lines: Vec<String> = original.lines().map(|l| l.to_string()).collect();
    let mut total_added = 0usize;
    let mut total_removed = 0usize;
    let mut hunks_applied = 0usize;
    let mut diff_parts = Vec::new();

    // Apply hunks in reverse order so line numbers don't shift
    for hunk in hunks.iter().rev() {
        if let Some((start, _)) = find_hunk_location(&lines, hunk) {
            // Build diff display
            for r in &hunk.removals {
                diff_parts.push(format!("- {r}"));
            }
            for a in &hunk.additions {
                diff_parts.push(format!("+ {a}"));
            }

            // Apply: remove old lines, insert new ones
            let remove_start = start + hunk.context_before.len();
            let remove_end = remove_start + hunk.removals.len();

            // Remove the old lines
            if remove_end <= lines.len() {
                lines.drain(remove_start..remove_end);
            }

            // Insert new lines
            for (i, addition) in hunk.additions.iter().enumerate() {
                lines.insert(remove_start + i, addition.clone());
            }

            total_added += hunk.additions.len();
            total_removed += hunk.removals.len();
            hunks_applied += 1;
        }
    }

    if hunks_applied == 0 {
        return None;
    }

    let patched = lines.join("\n");
    let summary = if diff_parts.is_empty() {
        format!("{hunks_applied} hunk(s) applied")
    } else {
        diff_parts.reverse(); // we applied in reverse, flip for display
        format!(
            "+{total_added} -{total_removed} lines, {hunks_applied} hunk(s)\n{}",
            diff_parts.join("\n")
        )
    };

    Some(PatchResult {
        patched_content: patched,
        diff_summary: summary,
        lines_added: total_added,
        lines_removed: total_removed,
        hunks_applied,
    })
}

fn parse_hunks(patch_text: &str) -> Option<Vec<Hunk>> {
    let mut hunks = Vec::new();
    let mut in_hunk = false;
    let mut context_before = Vec::new();
    let mut removals = Vec::new();
    let mut additions = Vec::new();
    let mut past_changes = false;

    for line in patch_text.lines() {
        let trimmed = line.trim_end();

        // Skip patch envelope lines
        if trimmed.starts_with("*** Begin Patch")
            || trimmed.starts_with("*** End Patch")
            || trimmed.starts_with("*** Update File")
            || trimmed.starts_with("*** Add File")
            || trimmed.starts_with("*** Delete File")
        {
            // If we were building a hunk, finish it
            if in_hunk && (!removals.is_empty() || !additions.is_empty()) {
                hunks.push(Hunk {
                    context_before: context_before.clone(),
                    removals: removals.clone(),
                    additions: additions.clone(),
                });
                context_before.clear();
                removals.clear();
                additions.clear();
                past_changes = false;
            }
            in_hunk = false;
            continue;
        }

        // Start of a new hunk
        if trimmed.starts_with("@@") {
            // Save previous hunk if any
            if in_hunk && (!removals.is_empty() || !additions.is_empty()) {
                hunks.push(Hunk {
                    context_before: context_before.clone(),
                    removals: removals.clone(),
                    additions: additions.clone(),
                });
            }
            context_before.clear();
            removals.clear();
            additions.clear();
            past_changes = false;
            in_hunk = true;
            continue;
        }

        if !in_hunk {
            continue;
        }

        if let Some(removed) = trimmed.strip_prefix('-') {
            removals.push(removed.to_string());
            past_changes = true;
        } else if let Some(added) = trimmed.strip_prefix('+') {
            additions.push(added.to_string());
            past_changes = true;
        } else if let Some(ctx) = trimmed.strip_prefix(' ') {
            if past_changes {
                // Context after changes -- finish this hunk
                hunks.push(Hunk {
                    context_before: context_before.clone(),
                    removals: removals.clone(),
                    additions: additions.clone(),
                });
                context_before.clear();
                context_before.push(ctx.to_string());
                removals.clear();
                additions.clear();
                past_changes = false;
            } else {
                context_before.push(ctx.to_string());
            }
        } else if !trimmed.is_empty() {
            // Unrecognized line -- treat as context
            if past_changes {
                hunks.push(Hunk {
                    context_before: context_before.clone(),
                    removals: removals.clone(),
                    additions: additions.clone(),
                });
                context_before.clear();
                context_before.push(trimmed.to_string());
                removals.clear();
                additions.clear();
                past_changes = false;
            } else {
                context_before.push(trimmed.to_string());
            }
        }
    }

    // Final hunk
    if in_hunk && (!removals.is_empty() || !additions.is_empty()) {
        hunks.push(Hunk {
            context_before,
            removals,
            additions,
        });
    }

    if hunks.is_empty() {
        None
    } else {
        Some(hunks)
    }
}

/// Find where a hunk should be applied in the file by matching context lines.
fn find_hunk_location(lines: &[String], hunk: &Hunk) -> Option<(usize, usize)> {
    if hunk.context_before.is_empty() && hunk.removals.is_empty() {
        return None;
    }

    let search_lines: Vec<&str> = hunk
        .context_before
        .iter()
        .chain(hunk.removals.iter())
        .map(|s| s.as_str())
        .collect();

    if search_lines.is_empty() {
        return None;
    }

    // Try to find the context + removals sequence in the file
    let window_size = search_lines.len();
    for i in 0..=lines.len().saturating_sub(window_size) {
        let mut matches = true;
        for (j, expected) in search_lines.iter().enumerate() {
            if i + j >= lines.len() {
                matches = false;
                break;
            }
            if lines[i + j].trim_end() != expected.trim_end() {
                matches = false;
                break;
            }
        }
        if matches {
            return Some((i, i + window_size));
        }
    }

    // Fuzzy fallback: try matching just the removal lines (ignoring context)
    if !hunk.removals.is_empty() {
        let removal_size = hunk.removals.len();
        for i in 0..=lines.len().saturating_sub(removal_size) {
            let mut matches = true;
            for (j, expected) in hunk.removals.iter().enumerate() {
                if i + j >= lines.len() {
                    matches = false;
                    break;
                }
                if lines[i + j].trim_end() != expected.trim_end() {
                    matches = false;
                    break;
                }
            }
            if matches {
                return Some((
                    i.saturating_sub(hunk.context_before.len()),
                    i + removal_size,
                ));
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_patch() {
        let original = "import Mathlib\n\ntheorem foo : True := by\n  trivial\n";
        let patch = r#"*** Begin Patch
*** Update File: Scratch.lean
@@ theorem foo
 theorem foo : True := by
-  trivial
+  exact trivial
*** End Patch"#;

        let result = apply_patch(original, patch).unwrap();
        assert!(result.patched_content.contains("exact trivial"));
        assert!(!result.patched_content.contains("  trivial\n"));
        assert_eq!(result.lines_added, 1);
        assert_eq!(result.lines_removed, 1);
    }

    #[test]
    fn test_extract_patch() {
        let text = "Here is the fix:\n*** Begin Patch\n*** Update File: Scratch.lean\n@@ foo\n-old\n+new\n*** End Patch\nDone.";
        let patch = extract_patch(text).unwrap();
        assert!(patch.starts_with("*** Begin Patch"));
        assert!(patch.ends_with("*** End Patch"));
    }

    #[test]
    fn test_no_patch() {
        assert!(extract_patch("just some text").is_none());
        assert!(!contains_patch("just some text"));
    }
}
