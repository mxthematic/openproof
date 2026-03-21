pub use rusqlite;

mod corpus;
mod corpus_seed;
mod corpus_sync;
mod extract;
mod schema;
mod sessions;
mod store;

pub use store::{AppStore, StorePaths};

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use openproof_protocol::{
        CloudPolicy, LeanVerificationSummary, ProofNode, ProofNodeKind, ProofNodeStatus,
        ProofSessionState, SessionSnapshot, ShareMode,
    };

    fn temp_paths(name: &str) -> StorePaths {
        let root = std::env::temp_dir().join(format!(
            "openproof-store-test-{}-{}",
            name,
            Utc::now().timestamp_millis()
        ));
        StorePaths {
            db_path: root.join("openproof-native.sqlite"),
            legacy_sessions_dir: root.join("legacy-sessions"),
            sessions_dir: root.join("workspaces"),
        }
    }

    fn sample_session() -> SessionSnapshot {
        let now = Utc::now().to_rfc3339();
        SessionSnapshot {
            id: "session_test".to_string(),
            title: "Store Test".to_string(),
            updated_at: now.clone(),
            workspace_root: Some("/tmp/openproof".to_string()),
            workspace_label: Some("openproof".to_string()),
            cloud: CloudPolicy {
                sync_enabled: true,
                share_mode: ShareMode::Community,
                private_overlay_community: false,
                last_sync_at: None,
            },
            transcript: Vec::new(),
            proof: ProofSessionState {
                phase: "proving".to_string(),
                status_line: "Working.".to_string(),
                root_node_id: Some("node_truth".to_string()),
                problem: None,
                formal_target: Some("True".to_string()),
                accepted_target: Some("True".to_string()),
                search_status: None,
                assumptions: Vec::new(),
                paper_notes: Vec::new(),
                pending_question: None,
                awaiting_clarification: false,
                is_autonomous_running: false,
                autonomous_iteration_count: 0,
                autonomous_started_at: None,
                autonomous_last_progress_at: None,
                autonomous_pause_reason: None,
                autonomous_stop_reason: None,
                hidden_best_branch_id: None,
                active_retrieval_summary: None,
                strategy_summary: None,
                goal_summary: Some("True".to_string()),
                latest_diagnostics: None,
                active_node_id: Some("node_truth".to_string()),
                active_branch_id: None,
                active_agent_role: None,
                active_foreground_branch_id: None,
                resolved_by_branch_id: None,
                hidden_branch_count: 0,
                imports: vec!["Mathlib".to_string()],
                nodes: vec![ProofNode {
                    id: "node_truth".to_string(),
                    kind: ProofNodeKind::Theorem,
                    label: "NativeTruth".to_string(),
                    statement: "True".to_string(),
                    content: "theorem NativeTruth : True := by\n  trivial".to_string(),
                    status: ProofNodeStatus::Verified,
                    created_at: now.clone(),
                    updated_at: now,
                }],
                branches: Vec::new(),
                agents: Vec::new(),
                last_rendered_scratch: None,
                last_verification: None,
                paper_tex: String::new(),
                scratch_path: None,
                paper_path: None,
                attempt_number: 0,
            },
        }
    }

    #[test]
    fn extracts_library_seed_items_from_lean_source() {
        let source = r#"
/-- A simple theorem. -/
theorem NativeTruth : True := by
  trivial

def helperValue : Nat :=
  1
"#;
        let items = corpus_seed::extract_library_seed_items(source);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].decl_name, "NativeTruth");
        assert_eq!(items[0].kind, "theorem");
        assert_eq!(items[0].doc_string.as_deref(), Some("A simple theorem."));
        assert_eq!(items[1].decl_name, "helperValue");
        assert_eq!(items[1].kind, "def");
    }

    #[test]
    fn successful_verification_enters_verified_corpus_and_sync_queue() {
        let store = AppStore::open(temp_paths("verified")).expect("open store");
        let session = sample_session();
        store.save_session(&session).expect("save session");
        let result = LeanVerificationSummary {
            ok: true,
            code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
            error: None,
            checked_at: Utc::now().to_rfc3339(),
            project_dir: "/tmp/openproof/lean".to_string(),
            scratch_path: "/tmp/openproof/Scratch.lean".to_string(),
            rendered_scratch: "import Mathlib".to_string(),
        };

        store
            .record_verification_result(&session, &result)
            .expect("record verification");

        let corpus = store.get_corpus_summary().expect("corpus summary");
        let sync = store.get_sync_summary().expect("sync summary");
        assert_eq!(corpus.verified_entry_count, 1);
        assert_eq!(corpus.user_verified_count, 1);
        assert_eq!(sync.pending_count, 1);
    }

    #[test]
    fn failed_verification_enters_attempt_memory_only() {
        let store = AppStore::open(temp_paths("attempt")).expect("open store");
        let session = sample_session();
        store.save_session(&session).expect("save session");
        let result = LeanVerificationSummary {
            ok: false,
            code: Some(1),
            stdout: String::new(),
            stderr: "type mismatch".to_string(),
            error: Some("type-mismatch".to_string()),
            checked_at: Utc::now().to_rfc3339(),
            project_dir: "/tmp/openproof/lean".to_string(),
            scratch_path: "/tmp/openproof/Scratch.lean".to_string(),
            rendered_scratch: "import Mathlib".to_string(),
        };

        store
            .record_verification_result(&session, &result)
            .expect("record verification");

        let corpus = store.get_corpus_summary().expect("corpus summary");
        let sync = store.get_sync_summary().expect("sync summary");
        assert_eq!(corpus.verified_entry_count, 0);
        assert_eq!(corpus.attempt_log_count, 1);
        assert_eq!(sync.pending_count, 0);
    }
}
