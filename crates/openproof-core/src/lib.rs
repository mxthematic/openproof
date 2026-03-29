mod apply;
mod apply_content;
mod apply_input;
mod apply_streaming;
mod commands;
pub mod decomposition_checks;
mod helpers;
mod parser;
mod proof;
mod reports;
mod session;
mod state;

pub use apply_content::derive_nogood_context;
pub use commands::{
    build_focus_items, command_completions, delete_word_backward_pos, SLASH_COMMANDS,
};
pub use helpers::default_session_with_workspace;
pub use parser::{
    derive_goal_label, extract_latex_block, extract_lean_code_block, extract_lean_code_blocks,
    parse_assistant_output, ParsedAssistantOutput,
};
pub use state::{
    AppEvent, AppState, AutonomousRunPatch, FocusPane, Overlay, PendingWrite, SubmittedInput,
};

#[cfg(test)]
mod tests {
    use super::*;
    use openproof_protocol::ProofNodeKind;

    #[test]
    fn parses_structured_assistant_output() {
        let parsed = parse_assistant_output(
            r#"
TITLE: Prime Gap Goal
PROBLEM: Show a normalized prime-gap subsequence limit exists.
FORMAL_TARGET: ∀ C : ℝ, 0 ≤ C → True
ACCEPTED_TARGET: ∀ C : ℝ, 0 ≤ C → True
PHASE: proving
STATUS: searching local lemmas
ASSUMPTION: C ≥ 0
QUESTION: Which normalization should we use?
OPTION: A | Log normalization | standard asymptotic target
OPTION_TARGET: A :: ∀ C : ℝ, 0 ≤ C → True
RECOMMENDED_OPTION: A
THEOREM: PrimeGapTarget :: ∀ C : ℝ, 0 ≤ C → True
LEMMA: helper_limit :: True
PAPER: We normalize by log n.
NEXT: verify the current candidate
```lean
theorem PrimeGapTarget : ∀ C : ℝ, 0 ≤ C → True := by
  intro C hC
  trivial
```
"#,
        );

        assert_eq!(parsed.title.as_deref(), Some("Prime Gap Goal"));
        assert_eq!(
            parsed.accepted_target.as_deref(),
            Some("∀ C : ℝ, 0 ≤ C → True")
        );
        assert_eq!(parsed.phase.as_deref(), Some("proving"));
        assert_eq!(parsed.created_nodes.len(), 2);
        assert_eq!(parsed.paper_notes.len(), 1);
        assert_eq!(parsed.lean_snippets.len(), 1);
        assert!(parsed.question.is_some());
        let question = parsed.question.unwrap();
        assert_eq!(question.prompt, "Which normalization should we use?");
        assert_eq!(question.options.len(), 1);
        assert_eq!(question.recommended_option_id.as_deref(), Some("A"));
    }

    #[test]
    fn append_assistant_updates_proof_state() {
        let mut state = AppState::new(
            vec![default_session_with_workspace(None, Some("openproof"))],
            "ready".to_string(),
            None,
            Some("openproof".to_string()),
        );
        let write = state.add_proof_node(
            ProofNodeKind::Theorem,
            "PrimeGapTarget",
            "∀ C : ℝ, 0 ≤ C → True",
        );
        assert!(write.is_ok());
        let _ = state.apply(AppEvent::AppendAssistant(
            r#"
TITLE: Prime Gap Goal
FORMAL_TARGET: ∀ C : ℝ, 0 ≤ C → True
ACCEPTED_TARGET: ∀ C : ℝ, 0 ≤ C → True
PAPER: We normalize by log n.
QUESTION: Which normalization should we use?
OPTION: A | Log normalization | standard asymptotic target
OPTION_TARGET: A :: ∀ C : ℝ, 0 ≤ C → True
RECOMMENDED_OPTION: A
```lean
theorem PrimeGapTarget : ∀ C : ℝ, 0 ≤ C → True := by
  intro C hC
  trivial
```
"#
            .to_string(),
        ));

        let session = state.current_session().unwrap();
        assert_eq!(session.title, "Prime Gap Goal");
        assert_eq!(
            session.proof.formal_target.as_deref(),
            Some("∀ C : ℝ, 0 ≤ C → True")
        );
        assert_eq!(
            session.proof.accepted_target.as_deref(),
            Some("∀ C : ℝ, 0 ≤ C → True")
        );
        assert_eq!(session.proof.paper_notes.len(), 1);
        assert_eq!(
            session
                .proof
                .nodes
                .first()
                .map(|node| node.content.contains("theorem PrimeGapTarget")),
            Some(true)
        );
    }

    #[test]
    fn question_selection_prefers_recommended_option() {
        let mut state = AppState::new(
            vec![default_session_with_workspace(None, Some("openproof"))],
            "ready".to_string(),
            None,
            Some("openproof".to_string()),
        );
        let _ = state.apply(AppEvent::AppendAssistant(
            r#"
QUESTION: Which target should we accept?
OPTION: A | Weak target | easier
OPTION_TARGET: A :: True
OPTION: B | Strong target | preferred
OPTION_TARGET: B :: ∀ n : ℕ, True
RECOMMENDED_OPTION: B
"#
            .to_string(),
        ));

        assert!(state.has_open_question());
        assert_eq!(
            state
                .selected_question_option()
                .map(|option| option.id.as_str()),
            Some("B")
        );
    }
}
