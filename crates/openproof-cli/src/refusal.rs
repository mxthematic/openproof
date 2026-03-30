//! Refusal detection and forced retry for LLM proof attempts.
//!
//! When the model recognizes a hard problem from training data (FrontierMath,
//! IMO, competitions), it may refuse to attempt it. This module detects refusal
//! signals in the model's text and provides retry messages to force continuation.

pub const MAX_REFUSAL_RETRIES: u8 = 2;

/// Strong refusal signals -- any single match triggers refusal detection.
const STRONG_SIGNALS: &[&str] = &[
    "open problem",
    "unsolved problem",
    "open question",
    "no known proof",
    "no known solution",
    "frontiermath",
    "frontier math",
    "competition problem",
    "benchmark problem",
    "beyond current capabilities",
    "beyond my capabilities",
    "beyond the capabilities",
    "i cannot solve this",
    "i'm unable to prove",
    "i am unable to prove",
    "cannot be proven",
    "is considered unsolved",
    "this is an open",
    "this problem is from",
    "known to be undecidable",
    "no elementary proof",
    "this is a well-known",
    "remains an open",
];

/// Weak refusal signals -- only trigger when combined with no tool usage
/// and no Lean code in the response.
const WEAK_SIGNALS: &[&str] = &[
    "i apologize",
    "i'm sorry",
    "i am sorry",
    "too difficult",
    "unable to",
    "cannot prove",
    "cannot solve",
    "i don't think",
    "i do not think",
    "not feasible",
    "not possible to prove",
];

/// Detect whether the model's response is a refusal to attempt the proof.
///
/// Returns `true` if the text contains refusal signals. If `used_tools` is true,
/// only strong signals trigger (the model at least tried before complaining).
pub fn detect_refusal(text: &str, used_tools: bool) -> bool {
    if text.trim().is_empty() {
        return false;
    }
    let lower = text.to_ascii_lowercase();

    // Strong signals always trigger.
    if STRONG_SIGNALS.iter().any(|s| lower.contains(s)) {
        return true;
    }

    // Weak signals only trigger if the model didn't use tools and wrote no Lean code.
    if !used_tools {
        let has_lean_code = text.contains("```lean");
        if !has_lean_code && WEAK_SIGNALS.iter().any(|s| lower.contains(s)) {
            return true;
        }
    }

    false
}

/// Build an escalating retry message to force the model to attempt the proof.
pub fn refusal_retry_message(retry_count: u8) -> String {
    match retry_count {
        1 => concat!(
            "You MUST attempt this proof. Do not assess difficulty or discuss problem sources. ",
            "Start by writing a sorry-skeleton: create a .lean file with the theorem statement ",
            "and `sorry` placeholders, then use lean_verify to check it compiles. ",
            "Decompose the problem into sub-have steps and fill them one at a time.",
        )
        .to_string(),
        _ => concat!(
            "FINAL ATTEMPT: Write Lean code NOW using file_write. Do not produce text without ",
            "tool calls. Create a sorry-skeleton immediately with the theorem statement and sorry. ",
            "Any response without a tool call wastes this proof attempt entirely.",
        )
        .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_strong_refusal() {
        assert!(detect_refusal(
            "This is an open problem in number theory.",
            false
        ));
        assert!(detect_refusal(
            "This problem is from FrontierMath and is unsolved.",
            false
        ));
        assert!(detect_refusal("I cannot solve this theorem.", false));
        assert!(detect_refusal(
            "The conjecture remains an open question.",
            false
        ));
    }

    #[test]
    fn detects_strong_even_with_tools() {
        assert!(detect_refusal("This is an open problem.", true));
    }

    #[test]
    fn detects_weak_refusal_without_tools() {
        assert!(detect_refusal(
            "I apologize, but this is too difficult for me.",
            false
        ));
        assert!(detect_refusal("I'm sorry, I cannot prove this.", false));
    }

    #[test]
    fn weak_signals_ignored_with_tools() {
        assert!(!detect_refusal(
            "I'm sorry, let me try a different approach.",
            true
        ));
    }

    #[test]
    fn weak_signals_ignored_with_lean_code() {
        let text =
            "I'm sorry, this is hard. Here's my attempt:\n```lean\ntheorem foo := sorry\n```";
        assert!(!detect_refusal(text, false));
    }

    #[test]
    fn no_refusal_on_normal_text() {
        assert!(!detect_refusal(
            "Let me try using norm_num for this.",
            false
        ));
        assert!(!detect_refusal("The proof follows from induction.", false));
        assert!(!detect_refusal("", false));
    }

    #[test]
    fn retry_messages_are_different() {
        let m1 = refusal_retry_message(1);
        let m2 = refusal_retry_message(2);
        assert_ne!(m1, m2);
        assert!(m1.contains("sorry-skeleton"));
        assert!(m2.contains("FINAL ATTEMPT"));
    }
}
