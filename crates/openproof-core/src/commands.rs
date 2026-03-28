use crate::state::AppState;

/// All known slash commands for tab completion.
pub const SLASH_COMMANDS: &[&str] = &[
    "help",
    "new",
    "resume",
    "nodes",
    "focus",
    "agent spawn",
    "proof",
    "lean",
    "paper",
    "answer",
    "memory",
    "remember",
    "share",
    "corpus status",
    "corpus search",
    "corpus ingest",
    "corpus recluster",
    "sync status",
    "sync enable",
    "sync disable",
    "sync drain",
    "export paper",
    "export tex",
    "export lean",
    "export all",
    "autonomous status",
    "autonomous start",
    "autonomous full",
    "autonomous stop",
    "autonomous step",
    "theorem",
    "lemma",
    "verify",
    "dashboard",
];

/// Compute tab completions for the current command buffer.
pub fn command_completions(input: &str) -> Vec<String> {
    SLASH_COMMANDS
        .iter()
        .filter(|c| c.starts_with(input))
        .map(|c| c.to_string())
        .collect()
}

/// Find the byte position after deleting one word backward from `cursor`.
///
/// Skips trailing whitespace, then skips the word, returning the byte offset
/// of the start of the word. Used for Ctrl+W / Alt+Backspace handling.
pub fn delete_word_backward_pos(text: &str, cursor: usize) -> usize {
    text[..cursor]
        .char_indices()
        .rev()
        .skip_while(|(_, c)| c.is_whitespace())
        .skip_while(|(_, c)| !c.is_whitespace())
        .map(|(i, c)| i + c.len_utf8())
        .next()
        .unwrap_or(0)
}

/// Build focusable targets (nodes + branches) for the focus picker.
pub fn build_focus_items(state: &AppState) -> Vec<(String, String, String)> {
    let mut items = Vec::new();
    if let Some(session) = state.current_session() {
        for node in &session.proof.nodes {
            let kind = format!("{:?}", node.kind).to_lowercase();
            items.push((node.id.clone(), node.label.clone(), kind));
        }
        for branch in &session.proof.branches {
            let kind = format!("branch/{}", branch.branch_kind);
            items.push((branch.id.clone(), branch.title.clone(), kind));
        }
    }
    items
}
