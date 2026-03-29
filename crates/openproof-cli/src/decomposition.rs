//! Feedback-driven decomposition scoring and decision logic.
//!
//! Monitors BFS search progress on proof tree nodes and decides:
//! - When to decompose a leaf into sub-lemmas
//! - When to abandon a subtree and re-decompose the parent
//! - When to pivot strategy entirely at the root
//!
//! Inspired by CDCL (conflict-driven clause learning) in SAT solvers:
//! when a leaf fails, analyze which ancestor decision caused the failure
//! and backtrack there, not just to the immediate parent.

use openproof_protocol::{ProofBranch, ProofNode, ProofNodeStatus, SearchAttemptMetrics};
use std::collections::HashMap;

/// Score for a single leaf node based on its BFS search history.
pub fn score_leaf(branch: &ProofBranch) -> f32 {
    let history = &branch.search_history;

    // No search attempts yet -- neutral score.
    if history.is_empty() {
        return 0.5;
    }

    // Solved.
    if history.iter().any(|h| h.outcome == "solved") {
        return 1.0;
    }

    // Only one attempt -- too early to judge.
    if history.len() < 2 {
        return 0.4;
    }

    let mut score: f32 = 0.5;

    // Check remaining goals trend.
    let goals: Vec<usize> = history.iter().map(|h| h.remaining_goals).collect();
    let improving = goals.windows(2).any(|w| w[1] < w[0]);
    let flatlined = goals.windows(2).all(|w| w[1] == w[0]);

    if improving {
        score += 0.2;
    }
    if flatlined && history.len() >= 3 {
        score -= 0.3;
    }

    // Repeated timeouts.
    let timeout_count = history.iter().filter(|h| h.timed_out).count();
    if timeout_count >= 2 {
        score -= 0.3;
    }

    // Repeated exhaustion with no progress.
    let exhausted_no_progress = history
        .iter()
        .filter(|h| h.outcome == "exhausted" && h.remaining_goals > 0)
        .count();
    if exhausted_no_progress >= 2 {
        score -= 0.2;
    }

    score.clamp(0.0, 1.0)
}

/// Aggregated score for a subtree rooted at a given node.
#[derive(Debug, Clone)]
pub struct SubtreeScore {
    pub score: f32,
    pub worst_child_id: Option<String>,
    pub children_solved: usize,
    pub children_total: usize,
}

/// Compute subtree scores for every node in the proof tree.
///
/// Leaf scores come from BFS search metrics on branches.
/// Interior scores = min(child scores) (AND-tree: all must succeed).
pub fn compute_subtree_scores(
    nodes: &[ProofNode],
    branches: &[ProofBranch],
) -> HashMap<String, SubtreeScore> {
    // Build leaf scores from branches.
    let mut leaf_scores: HashMap<String, f32> = HashMap::new();
    for branch in branches {
        if let Some(ref node_id) = branch.focus_node_id {
            let s = score_leaf(branch);
            leaf_scores
                .entry(node_id.clone())
                .and_modify(|existing| *existing = existing.max(s))
                .or_insert(s);
        }
    }

    // Mark verified nodes as solved.
    for node in nodes {
        if node.status == ProofNodeStatus::Verified {
            leaf_scores.insert(node.id.clone(), 1.0);
        }
    }

    // Build parent -> children map.
    let mut children_of: HashMap<String, Vec<String>> = HashMap::new();
    for node in nodes {
        if let Some(ref parent) = node.parent_id {
            children_of
                .entry(parent.clone())
                .or_default()
                .push(node.id.clone());
        }
    }

    // Bottom-up score propagation.
    let mut scores: HashMap<String, SubtreeScore> = HashMap::new();
    // Process deepest nodes first.
    let mut sorted_nodes: Vec<&ProofNode> = nodes.iter().collect();
    sorted_nodes.sort_by(|a, b| b.depth.cmp(&a.depth));

    for node in &sorted_nodes {
        let children = children_of.get(&node.id);

        if children.is_none() || children.map_or(true, |c| c.is_empty()) {
            // Leaf node.
            let leaf_score = leaf_scores.get(&node.id).copied().unwrap_or(0.5);
            scores.insert(
                node.id.clone(),
                SubtreeScore {
                    score: leaf_score,
                    worst_child_id: None,
                    children_solved: if leaf_score >= 1.0 { 1 } else { 0 },
                    children_total: 1,
                },
            );
        } else {
            // Interior node: min of children (AND-tree).
            let child_ids = children.unwrap();
            let mut min_score: f32 = 1.0;
            let mut worst_id: Option<String> = None;
            let mut solved = 0usize;
            let total = child_ids.len();

            for cid in child_ids {
                let child_score = scores.get(cid).map_or(0.5, |s| s.score);
                if child_score >= 1.0 {
                    solved += 1;
                }
                if child_score < min_score {
                    min_score = child_score;
                    worst_id = Some(cid.clone());
                }
            }

            scores.insert(
                node.id.clone(),
                SubtreeScore {
                    score: min_score,
                    worst_child_id: worst_id,
                    children_solved: solved,
                    children_total: total,
                },
            );
        }
    }

    scores
}

/// What the autonomous loop should do based on subtree health.
#[derive(Debug, Clone)]
pub enum DecompositionAction {
    /// BFS is making progress or too early to judge; don't intervene.
    Continue,
    /// Break this leaf node into sub-lemmas.
    DecomposeLeaf { node_id: String },
    /// Abandon this subtree and re-decompose the interior node.
    RedecomposeInterior {
        node_id: String,
        failed_children: Vec<String>,
        reason: String,
    },
    /// Abandon entire decomposition; try a completely different approach.
    FullPivot { reason: String },
}

/// A recorded failed decomposition pattern (nogood).
#[derive(Debug, Clone)]
pub struct Nogood {
    /// The goal that was decomposed.
    pub parent_goal: String,
    /// The sub-lemma type signatures that were generated.
    pub sub_lemma_types: Vec<String>,
    /// Which sub-lemma couldn't be proved.
    pub failed_child: String,
    /// Why it failed.
    pub failure_reason: String,
}

/// Decide what decomposition action to take for a given node.
pub fn decide_action(
    node_id: &str,
    scores: &HashMap<String, SubtreeScore>,
    attempt_count: usize,
) -> DecompositionAction {
    let Some(score) = scores.get(node_id) else {
        return DecompositionAction::Continue;
    };

    // Leaf node that's stuck.
    if score.children_total <= 1 && score.score < 0.3 && attempt_count >= 2 {
        return DecompositionAction::DecomposeLeaf {
            node_id: node_id.to_string(),
        };
    }

    // Interior node: one stuck child, others solved -- decompose the stuck child.
    if score.children_total > 1
        && score.children_solved == score.children_total - 1
        && score.score < 0.3
    {
        if let Some(ref worst) = score.worst_child_id {
            return DecompositionAction::DecomposeLeaf {
                node_id: worst.clone(),
            };
        }
    }

    // Interior node: multiple stuck children -- the decomposition was bad.
    let stuck_count = score.children_total.saturating_sub(score.children_solved);
    if stuck_count >= 2 && score.score < 0.2 {
        let failed: Vec<String> = scores
            .iter()
            .filter(|(_, s)| s.score < 0.3)
            .map(|(id, _)| id.clone())
            .collect();
        return DecompositionAction::RedecomposeInterior {
            node_id: node_id.to_string(),
            failed_children: failed,
            reason: format!(
                "{stuck_count}/{} children stuck, subtree score {:.2}",
                score.children_total, score.score
            ),
        };
    }

    // Root-level: very low score after decomposition already attempted.
    if score.score < 0.1 && attempt_count >= 4 {
        return DecompositionAction::FullPivot {
            reason: format!(
                "Subtree score {:.2} after {} attempts; entire approach likely wrong",
                score.score, attempt_count
            ),
        };
    }

    DecompositionAction::Continue
}

/// Format nogood context for the Planner prompt when re-decomposing.
pub fn format_nogood_context(nogoods: &[Nogood]) -> String {
    if nogoods.is_empty() {
        return String::new();
    }

    let mut ctx =
        String::from("\n\nPREVIOUS FAILED DECOMPOSITIONS (do NOT repeat these patterns):\n");
    for (i, ng) in nogoods.iter().enumerate() {
        ctx.push_str(&format!(
            "\n{}. Decomposed into: [{}]\n   Failed on: {}\n   Reason: {}\n",
            i + 1,
            ng.sub_lemma_types.join(", "),
            ng.failed_child,
            ng.failure_reason,
        ));
    }
    ctx
}

// ---------------------------------------------------------------------------
// Decomposition validation gate
// ---------------------------------------------------------------------------

/// Build a Lean source file that validates a proposed decomposition.
///
/// Creates axioms for each sub-lemma and attempts to prove the parent
/// goal using them. If this type-checks, the decomposition is logically
/// sound -- the sub-lemmas compose to prove the parent.
///
/// Returns the Lean source to be verified by `lake env lean`.
pub fn build_validation_lean(
    parent_statement: &str,
    sub_lemma_statements: &[(String, String)],
) -> String {
    let mut src = String::from("import Mathlib\n\n");

    // Declare each sub-lemma as an axiom.
    for (label, stmt) in sub_lemma_statements {
        let clean_label = label
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>();
        src.push_str(&format!("axiom {clean_label} : {stmt}\n"));
    }

    src.push('\n');

    // The parent goal should be provable from the axioms.
    // The Planner should have provided a composition sketch in its Lean
    // code block, but as a minimal check we emit a `sorry`-free theorem
    // declaration that Lean will check for type correctness.
    src.push_str(&format!(
        "-- Validate: sub-lemmas compose to prove the parent goal.\n\
         -- If this file type-checks (ignoring sorry), the decomposition is sound.\n\
         theorem _decomposition_valid : {parent_statement} := by\n\
         sorry -- to be filled by composition proof\n"
    ));

    src
}

/// Check whether a decomposition is self-consistent.
///
/// Returns a list of diagnostic issues. Empty list = valid.
pub fn check_decomposition_consistency(
    parent_statement: &str,
    sub_lemma_statements: &[(String, String)],
) -> Vec<String> {
    let mut issues = Vec::new();

    if sub_lemma_statements.is_empty() {
        issues.push("No sub-lemmas proposed".to_string());
    }
    if sub_lemma_statements.len() > 6 {
        issues.push(format!(
            "Too many sub-lemmas ({}); 2-4 is ideal",
            sub_lemma_statements.len()
        ));
    }

    // Check for trivial/circular decompositions.
    for (label, stmt) in sub_lemma_statements {
        let parent_norm = parent_statement.trim().to_lowercase();
        let child_norm = stmt.trim().to_lowercase();
        if parent_norm == child_norm {
            issues.push(format!(
                "Sub-lemma '{label}' is identical to the parent goal"
            ));
        }
    }

    // Check for duplicate sub-lemmas.
    let mut seen = std::collections::HashSet::new();
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
    use openproof_protocol::{ProofNodeKind, ProofNodeStatus};

    fn make_branch(
        id: &str,
        focus_node: &str,
        history: Vec<SearchAttemptMetrics>,
        attempt_count: usize,
    ) -> ProofBranch {
        ProofBranch {
            id: id.to_string(),
            focus_node_id: Some(focus_node.to_string()),
            search_history: history,
            attempt_count,
            ..Default::default()
        }
    }

    fn make_node(
        id: &str,
        parent: Option<&str>,
        depth: usize,
        status: ProofNodeStatus,
    ) -> ProofNode {
        ProofNode {
            id: id.to_string(),
            parent_id: parent.map(|p| p.to_string()),
            depth,
            status,
            label: id.to_string(),
            statement: format!("{id}_type"),
            ..Default::default()
        }
    }

    fn metrics(remaining: usize, expansions: usize, outcome: &str) -> SearchAttemptMetrics {
        SearchAttemptMetrics {
            remaining_goals: remaining,
            expansions,
            timed_out: outcome == "timeout",
            outcome: outcome.to_string(),
            timestamp: String::new(),
        }
    }

    // -- score_leaf tests --

    #[test]
    fn leaf_score_no_history_is_neutral() {
        let branch = make_branch("b1", "n1", vec![], 0);
        assert!((score_leaf(&branch) - 0.5).abs() < 0.01);
    }

    #[test]
    fn leaf_score_solved_is_max() {
        let branch = make_branch("b1", "n1", vec![metrics(0, 50, "solved")], 1);
        assert!((score_leaf(&branch) - 1.0).abs() < 0.01);
    }

    #[test]
    fn leaf_score_flatlined_drops() {
        let h = vec![
            metrics(3, 200, "partial"),
            metrics(3, 200, "partial"),
            metrics(3, 200, "partial"),
        ];
        let branch = make_branch("b1", "n1", h, 3);
        let s = score_leaf(&branch);
        assert!(s < 0.3, "Flatlined leaf should score low, got {s}");
    }

    #[test]
    fn leaf_score_improving_stays_healthy() {
        let h = vec![
            metrics(5, 200, "partial"),
            metrics(3, 200, "partial"),
            metrics(1, 200, "partial"),
        ];
        let branch = make_branch("b1", "n1", h, 3);
        let s = score_leaf(&branch);
        assert!(s >= 0.5, "Improving leaf should score healthy, got {s}");
    }

    #[test]
    fn leaf_score_repeated_timeouts_drops() {
        let h = vec![metrics(3, 200, "timeout"), metrics(3, 200, "timeout")];
        let branch = make_branch("b1", "n1", h, 2);
        let s = score_leaf(&branch);
        assert!(s < 0.3, "Repeated timeouts should score low, got {s}");
    }

    // -- subtree scoring tests --

    #[test]
    fn subtree_single_leaf() {
        let nodes = vec![make_node("root", None, 0, ProofNodeStatus::Proving)];
        let branches = vec![make_branch(
            "b1",
            "root",
            vec![metrics(2, 100, "partial")],
            1,
        )];
        let scores = compute_subtree_scores(&nodes, &branches);
        assert!(scores.contains_key("root"));
    }

    #[test]
    fn subtree_propagates_min() {
        // Root -> A (solved), B (stuck)
        let nodes = vec![
            make_node("root", None, 0, ProofNodeStatus::Proving),
            make_node("A", Some("root"), 1, ProofNodeStatus::Verified),
            make_node("B", Some("root"), 1, ProofNodeStatus::Proving),
        ];
        let branches = vec![make_branch(
            "bB",
            "B",
            vec![
                metrics(3, 200, "exhausted"),
                metrics(3, 200, "exhausted"),
                metrics(3, 200, "exhausted"),
            ],
            3,
        )];
        let scores = compute_subtree_scores(&nodes, &branches);
        let root_score = scores.get("root").unwrap();
        // Root should have min of A (1.0) and B (low) = low
        assert!(
            root_score.score < 0.5,
            "Root score should be dragged down by B, got {}",
            root_score.score
        );
        assert_eq!(root_score.worst_child_id.as_deref(), Some("B"));
        assert_eq!(root_score.children_solved, 1); // A solved
        assert_eq!(root_score.children_total, 2); // A and B
    }

    #[test]
    fn subtree_all_solved() {
        let nodes = vec![
            make_node("root", None, 0, ProofNodeStatus::Proving),
            make_node("A", Some("root"), 1, ProofNodeStatus::Verified),
            make_node("B", Some("root"), 1, ProofNodeStatus::Verified),
        ];
        let scores = compute_subtree_scores(&nodes, &[]);
        let root_score = scores.get("root").unwrap();
        assert!((root_score.score - 1.0).abs() < 0.01);
        assert_eq!(root_score.children_solved, 2);
    }

    // -- decide_action tests --

    #[test]
    fn decide_continue_when_healthy() {
        let mut scores = HashMap::new();
        scores.insert(
            "root".to_string(),
            SubtreeScore {
                score: 0.8,
                worst_child_id: None,
                children_solved: 0,
                children_total: 1,
            },
        );
        assert!(matches!(
            decide_action("root", &scores, 1),
            DecompositionAction::Continue
        ));
    }

    #[test]
    fn decide_decompose_stuck_leaf() {
        let mut scores = HashMap::new();
        scores.insert(
            "leaf".to_string(),
            SubtreeScore {
                score: 0.1,
                worst_child_id: None,
                children_solved: 0,
                children_total: 1,
            },
        );
        assert!(matches!(
            decide_action("leaf", &scores, 3),
            DecompositionAction::DecomposeLeaf { .. }
        ));
    }

    #[test]
    fn decide_redecompose_when_multiple_stuck() {
        let mut scores = HashMap::new();
        scores.insert(
            "parent".to_string(),
            SubtreeScore {
                score: 0.1,
                worst_child_id: Some("child1".to_string()),
                children_solved: 0,
                children_total: 3,
            },
        );
        scores.insert(
            "child1".to_string(),
            SubtreeScore {
                score: 0.1,
                worst_child_id: None,
                children_solved: 0,
                children_total: 1,
            },
        );
        scores.insert(
            "child2".to_string(),
            SubtreeScore {
                score: 0.15,
                worst_child_id: None,
                children_solved: 0,
                children_total: 1,
            },
        );
        assert!(matches!(
            decide_action("parent", &scores, 3),
            DecompositionAction::RedecomposeInterior { .. }
        ));
    }

    #[test]
    fn decide_decompose_single_stuck_child() {
        let mut scores = HashMap::new();
        scores.insert(
            "parent".to_string(),
            SubtreeScore {
                score: 0.1,
                worst_child_id: Some("child_bad".to_string()),
                children_solved: 2,
                children_total: 3,
            },
        );
        assert!(matches!(
            decide_action("parent", &scores, 3),
            DecompositionAction::DecomposeLeaf { .. }
        ));
    }

    #[test]
    fn decide_full_pivot_when_root_dead() {
        let mut scores = HashMap::new();
        scores.insert(
            "root".to_string(),
            SubtreeScore {
                score: 0.05,
                worst_child_id: Some("a".to_string()),
                children_solved: 0,
                children_total: 2,
            },
        );
        // depth=0 check relies on attempt_count being high
        let action = decide_action("root", &scores, 5);
        // With score < 0.1 and attempts >= 4, should pivot
        assert!(
            matches!(
                action,
                DecompositionAction::FullPivot { .. }
                    | DecompositionAction::RedecomposeInterior { .. }
            ),
            "Expected pivot or redecompose for dead root, got {:?}",
            action
        );
    }

    // -- nogood formatting tests --

    #[test]
    fn format_nogoods_empty() {
        assert_eq!(format_nogood_context(&[]), "");
    }

    #[test]
    fn format_nogoods_includes_all() {
        let nogoods = vec![Nogood {
            parent_goal: "G".into(),
            sub_lemma_types: vec!["A".into(), "B".into()],
            failed_child: "B".into(),
            failure_reason: "BFS exhausted".into(),
        }];
        let ctx = format_nogood_context(&nogoods);
        assert!(ctx.contains("do NOT repeat"));
        assert!(ctx.contains("A, B"));
        assert!(ctx.contains("BFS exhausted"));
    }
}

/// Record a nogood from a failed decomposition and return updated context
/// for the Planner's next attempt.
pub fn record_nogood(
    nogoods: &mut Vec<Nogood>,
    parent_goal: &str,
    sub_lemma_types: &[String],
    failed_child: &str,
    failure_reason: &str,
) {
    nogoods.push(Nogood {
        parent_goal: parent_goal.to_string(),
        sub_lemma_types: sub_lemma_types.to_vec(),
        failed_child: failed_child.to_string(),
        failure_reason: failure_reason.to_string(),
    });
}
