//! Render Lean source from proof nodes and session state.

use openproof_protocol::{ProofNode, SessionSnapshot};

/// Render a ProofNode's content as a complete Lean scratch file with imports.
/// Includes all sibling nodes so helper lemmas are available to the active node.
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

    // Include all sibling nodes first (dependency order: other nodes before active).
    // Dedup by label to avoid rendering the same declaration twice.
    let mut seen_labels = std::collections::BTreeSet::new();
    for sibling in &session.proof.nodes {
        if sibling.id == node.id || sibling.content.trim().is_empty() {
            continue;
        }
        if !seen_labels.insert(sibling.label.clone()) {
            continue;
        }
        let sibling_content = clean_lean_content(sibling.content.trim());
        let sibling_content = sibling_content.trim();
        if sibling_content.is_empty() || sibling_content.starts_with("import ") {
            continue;
        }
        lines.push(format!("-- openproof: {} :: {}", escape_comment(&sibling.label), escape_comment(&sibling.statement)));
        lines.push(sibling_content.to_string());
        lines.push(String::new());
    }

    lines.push(format!("-- openproof: {} :: {}", escape_comment(&node.label), escape_comment(&node.statement)));
    lines.push(String::new());
    lines.push(content.to_string());
    lines.join("\n")
}

/// Strip openproof structured markers that may have leaked into Lean code.
fn clean_lean_content(content: &str) -> String {
    content
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
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

pub(crate) fn dedup_strings(values: Vec<String>) -> Vec<String> {
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
