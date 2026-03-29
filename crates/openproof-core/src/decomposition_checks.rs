//! Pure decomposition validation logic usable from apply_content.
//!
//! These are synchronous checks that don't require Lean or async I/O.

use std::collections::HashSet;

/// Check whether a proposed decomposition is self-consistent.
///
/// Returns a list of issues. Empty list = looks valid.
pub fn check_decomposition_consistency(
    parent_statement: &str,
    sub_lemma_statements: &[(String, String)],
) -> Vec<String> {
    let mut issues = Vec::new();

    if sub_lemma_statements.is_empty() {
        issues.push("No sub-lemmas proposed".to_string());
        return issues;
    }
    if sub_lemma_statements.len() > 6 {
        issues.push(format!(
            "Too many sub-lemmas ({}); 2-4 is ideal",
            sub_lemma_statements.len()
        ));
    }

    // Check for circular decomposition (sub-lemma identical to parent).
    let parent_norm = parent_statement.trim().to_lowercase();
    for (label, stmt) in sub_lemma_statements {
        let child_norm = stmt.trim().to_lowercase();
        if !parent_norm.is_empty() && parent_norm == child_norm {
            issues.push(format!(
                "Sub-lemma '{label}' is identical to the parent goal"
            ));
        }
    }

    // Check for duplicate sub-lemmas.
    let mut seen = HashSet::new();
    for (label, stmt) in sub_lemma_statements {
        let key = stmt.trim().to_lowercase();
        if !seen.insert(key) {
            issues.push(format!("Duplicate sub-lemma: '{label}'"));
        }
    }

    issues
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_decomposition() {
        let issues = check_decomposition_consistency(
            "a + b = b + a",
            &[
                ("comm_add".into(), "a + b = b + a -> True".into()),
                ("helper".into(), "b + a = a + b".into()),
            ],
        );
        assert!(issues.is_empty(), "Expected no issues, got: {:?}", issues);
    }

    #[test]
    fn catches_circular_decomposition() {
        let issues = check_decomposition_consistency(
            "a + b = b + a",
            &[("circ".into(), "a + b = b + a".into())],
        );
        assert!(issues.iter().any(|i| i.contains("identical")));
    }

    #[test]
    fn catches_duplicate_sub_lemmas() {
        let issues = check_decomposition_consistency(
            "parent",
            &[
                ("lem1".into(), "x = y".into()),
                ("lem2".into(), "x = y".into()),
            ],
        );
        assert!(issues.iter().any(|i| i.contains("Duplicate")));
    }

    #[test]
    fn catches_too_many_sub_lemmas() {
        let many: Vec<(String, String)> = (0..8)
            .map(|i| (format!("lem{i}"), format!("stmt{i}")))
            .collect();
        let issues = check_decomposition_consistency("parent", &many);
        assert!(issues.iter().any(|i| i.contains("Too many")));
    }

    #[test]
    fn catches_empty_decomposition() {
        let issues = check_decomposition_consistency("parent", &[]);
        assert!(issues.iter().any(|i| i.contains("No sub-lemmas")));
    }
}
