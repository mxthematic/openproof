use anyhow::Result;
use chrono::Utc;
use openproof_protocol::{
    CorpusSummary, LeanVerificationSummary, ProofNodeKind, SessionSnapshot, ShareMode,
};
use rusqlite::params;
use std::collections::BTreeMap;

use crate::store::AppStore;

// ---------------------------------------------------------------------------
// Shared utility functions (used by corpus_seed and corpus_sync as well)
// ---------------------------------------------------------------------------

pub(crate) fn next_store_id(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let ts = Utc::now().timestamp_millis();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}_{ts}_{seq}")
}

pub fn stable_hash(input: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

pub fn sanitize_identity_segment(input: &str) -> String {
    let mut value = input
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    while value.contains("__") {
        value = value.replace("__", "_");
    }
    value.trim_matches('_').to_string()
}

pub(crate) fn share_mode_to_str(mode: ShareMode) -> &'static str {
    match mode {
        ShareMode::Local => "local",
        ShareMode::Community => "community",
        ShareMode::Private => "private",
    }
}

fn classify_failure(result: &LeanVerificationSummary) -> String {
    let combined = format!(
        "{}\n{}\n{}",
        result.stderr,
        result.stdout,
        result.error.clone().unwrap_or_default()
    )
    .to_ascii_lowercase();
    if combined.contains("unknown constant") || combined.contains("unknown identifier") {
        "unknown-identifier".to_string()
    } else if combined.contains("type mismatch") {
        "type-mismatch".to_string()
    } else if combined.contains("application type mismatch") {
        "application-type-mismatch".to_string()
    } else if combined.contains("unsolved goals") {
        "unsolved-goals".to_string()
    } else if combined.contains("sorry") {
        "sorry-placeholder".to_string()
    } else if combined.contains("timeout") {
        "timeout".to_string()
    } else if let Some(error) = result.error.as_ref().filter(|value| !value.trim().is_empty()) {
        error.trim().to_string()
    } else {
        "lean-error".to_string()
    }
}

pub(crate) fn summarize_lean_diagnostic(result: &LeanVerificationSummary) -> String {
    let primary = if !result.stderr.trim().is_empty() {
        result.stderr.trim()
    } else if !result.stdout.trim().is_empty() {
        result.stdout.trim()
    } else {
        result.error.as_deref().unwrap_or("Lean verification failed.")
    };
    primary.lines().take(12).collect::<Vec<_>>().join("\n")
}

// ---------------------------------------------------------------------------
// Cluster helpers (used only by rebuild_verified_corpus_clusters)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct CorpusClusterRecord {
    id: String,
    cluster_key: String,
    canonical_item_id: String,
    label: String,
    statement_preview: String,
    member_count: usize,
    created_at: String,
    updated_at: String,
}

fn normalize_statement_for_cluster(statement: &str) -> String {
    statement
        .chars()
        .flat_map(|ch| ch.to_lowercase())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace(" :", ":")
        .replace(": ", ":")
        .replace(" ,", ",")
        .replace(", ", ",")
        .replace(" (", "(")
        .replace("( ", "(")
        .replace(" )", ")")
        .replace(" = ", "=")
        .replace(" + ", "+")
        .replace(" - ", "-")
        .replace(" * ", "*")
        .replace(" / ", "/")
        .trim()
        .to_string()
}

fn compute_corpus_cluster_key(
    statement: &str,
    decl_kind: &str,
    is_theorem_like: bool,
    content_hash: &str,
) -> String {
    if !is_theorem_like && !content_hash.trim().is_empty() {
        return stable_hash(&format!("artifact::{decl_kind}::{content_hash}"));
    }
    stable_hash(&format!(
        "{}::{}::{}",
        if is_theorem_like {
            "theorem-like"
        } else {
            "declaration"
        },
        decl_kind,
        normalize_statement_for_cluster(statement)
    ))
}

fn preview_text(value: &str, limit: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= limit {
        return trimmed.to_string();
    }
    trimmed.chars().take(limit).collect::<String>()
}

// ---------------------------------------------------------------------------
// AppStore impl: verification recording, cluster rebuild, corpus queries
// ---------------------------------------------------------------------------

impl AppStore {
    pub fn record_verification_result(
        &self,
        session: &SessionSnapshot,
        result: &LeanVerificationSummary,
    ) -> Result<()> {
        let Some(active_node_id) = session.proof.active_node_id.as_deref() else {
            return Ok(());
        };
        let Some(node) = session
            .proof
            .nodes
            .iter()
            .find(|node| node.id == active_node_id)
        else {
            return Ok(());
        };

        let conn = self.connect()?;
        let tx = conn.unchecked_transaction()?;
        let now = result.checked_at.clone();

        // Build self-contained artifact content: imports + all helper declarations + main theorem.
        // This ensures the artifact can be compiled standalone without missing dependencies.
        let self_contained_content = {
            let mut parts = String::new();
            // Include sibling nodes (helpers/lemmas the main theorem depends on)
            let mut seen_labels = std::collections::BTreeSet::new();
            for sibling in &session.proof.nodes {
                if sibling.id == node.id || sibling.content.trim().is_empty() {
                    continue;
                }
                if !seen_labels.insert(sibling.label.clone()) {
                    continue;
                }
                let clean = sibling.content.lines()
                    .filter(|l| !l.trim().starts_with("import ") && !l.trim().starts_with("open "))
                    .collect::<Vec<_>>().join("\n");
                if !clean.trim().is_empty() {
                    parts.push_str(clean.trim());
                    parts.push_str("\n\n");
                }
            }
            // Add the active node
            let clean_node = node.content.lines()
                .filter(|l| !l.trim().starts_with("import ") && !l.trim().starts_with("open "))
                .collect::<Vec<_>>().join("\n");
            parts.push_str(clean_node.trim());
            parts
        };

        let content_hash = stable_hash(&self_contained_content);
        let artifact_id = format!("artifact_{}", content_hash);
        tx.execute(
            r#"
            INSERT INTO verified_artifacts
            (id, artifact_hash, label, content, imports_json, namespace, metadata_json, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(artifact_hash) DO UPDATE SET
                label = excluded.label,
                content = excluded.content,
                imports_json = excluded.imports_json,
                namespace = excluded.namespace,
                metadata_json = excluded.metadata_json,
                updated_at = excluded.updated_at
            "#,
            params![
                artifact_id,
                content_hash,
                node.label,
                self_contained_content,
                serde_json::to_string(&session.proof.imports)?,
                Option::<String>::None,
                serde_json::to_string(&serde_json::json!({
                    "workspaceRoot": session.workspace_root,
                    "workspaceLabel": session.workspace_label
                }))?,
                now,
                now,
            ],
        )?;

        let verification_run_id = next_store_id("verification");
        tx.execute(
            r#"
            INSERT INTO verification_runs
            (id, session_id, target_kind, target_id, target_label, target_node_id, artifact_id, ok, code, stdout, stderr, error, scratch_path, rendered_scratch, created_at)
            VALUES (?, ?, 'node', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
            params![
                verification_run_id,
                session.id,
                node.id,
                node.label,
                node.id,
                artifact_id,
                if result.ok { 1 } else { 0 },
                result.code,
                result.stdout,
                result.stderr,
                result.error,
                result.scratch_path,
                result.rendered_scratch,
                now,
            ],
        )?;

        if result.ok {
            let identity_key = format!(
                "user-verified/{}/{}/{}",
                sanitize_identity_segment(session.id.as_str()),
                sanitize_identity_segment(node.label.as_str()),
                stable_hash(node.statement.as_str())
            );
            let visibility = share_mode_to_str(session.cloud.share_mode);
            tx.execute(
                r#"
                INSERT INTO verified_corpus_items
                (id, statement_hash, identity_key, label, statement, content_hash, artifact_id, verification_run_id, visibility, decl_kind, search_text, origin, namespace, imports_json, metadata_json, source_session_id, source_node_id, created_at, updated_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'user-verified', ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(identity_key) DO UPDATE SET
                    label = excluded.label,
                    statement = excluded.statement,
                    content_hash = excluded.content_hash,
                    artifact_id = excluded.artifact_id,
                    verification_run_id = excluded.verification_run_id,
                    visibility = excluded.visibility,
                    search_text = excluded.search_text,
                    imports_json = excluded.imports_json,
                    metadata_json = excluded.metadata_json,
                    updated_at = excluded.updated_at
                "#,
                params![
                    next_store_id("corpus"),
                    stable_hash(node.statement.as_str()),
                    identity_key,
                    node.label,
                    node.statement,
                    content_hash,
                    artifact_id,
                    verification_run_id,
                    visibility,
                    match node.kind {
                        ProofNodeKind::Lemma => "lemma",
                        ProofNodeKind::Theorem => "theorem",
                        ProofNodeKind::Artifact => "artifact",
                        ProofNodeKind::Attempt => "attempt",
                        ProofNodeKind::Conjecture => "conjecture",
                    },
                    format!("{} {} {}", node.label, node.statement, node.content),
                    Option::<String>::None,
                    serde_json::to_string(&session.proof.imports)?,
                    serde_json::to_string(&serde_json::json!({
                        "kind": format!("{:?}", node.kind),
                        "workspaceRoot": session.workspace_root,
                        "workspaceLabel": session.workspace_label
                    }))?,
                    session.id,
                    node.id,
                    now,
                    now,
                ],
            )?;

            // Store ALL sibling nodes (helper lemmas, defs) as separate corpus entries.
            // This ensures helper lemmas like Zsqrtd_norm_ne_two are individually discoverable.
            for sibling in &session.proof.nodes {
                if sibling.id == node.id || sibling.content.trim().is_empty() {
                    continue;
                }
                let sib_clean = sibling.content.lines()
                    .filter(|l| !l.trim().starts_with("import ") && !l.trim().starts_with("open "))
                    .collect::<Vec<_>>().join("\n");
                let sib_clean = sib_clean.trim();
                if sib_clean.is_empty() {
                    continue;
                }
                let sib_content_hash = stable_hash(sib_clean);
                let sib_artifact_id = format!("artifact_{}", sib_content_hash);
                // Insert artifact for sibling
                let _ = tx.execute(
                    r#"
                    INSERT OR IGNORE INTO verified_artifacts
                    (id, artifact_hash, label, content, imports_json, namespace, metadata_json, created_at, updated_at)
                    VALUES (?, ?, ?, ?, ?, ?, '{}', ?, ?)
                    "#,
                    params![
                        &sib_artifact_id, &sib_content_hash, sibling.label, sib_clean,
                        serde_json::to_string(&session.proof.imports)?,
                        Option::<String>::None, &now, &now,
                    ],
                );
                let sib_identity_key = format!(
                    "user-verified/{}/{}/{}",
                    sanitize_identity_segment(session.id.as_str()),
                    sanitize_identity_segment(sibling.label.as_str()),
                    stable_hash(sibling.statement.as_str())
                );
                let sib_kind = match sibling.kind {
                    ProofNodeKind::Lemma => "lemma",
                    ProofNodeKind::Theorem => "theorem",
                    _ => "def",
                };
                let _ = tx.execute(
                    r#"
                    INSERT INTO verified_corpus_items
                    (id, statement_hash, identity_key, label, statement, content_hash, artifact_id, verification_run_id, visibility, decl_kind, search_text, origin, imports_json, metadata_json, source_session_id, source_node_id, created_at, updated_at)
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'user-verified', ?, '{}', ?, ?, ?, ?)
                    ON CONFLICT(identity_key) DO NOTHING
                    "#,
                    params![
                        next_store_id("corpus"), stable_hash(sibling.statement.as_str()),
                        sib_identity_key, sibling.label, sibling.statement,
                        sib_content_hash, sib_artifact_id, &verification_run_id,
                        &visibility, sib_kind,
                        format!("{} {} {}", sibling.label, sibling.statement, sib_clean),
                        serde_json::to_string(&session.proof.imports)?,
                        session.id, sibling.id, &now, &now,
                    ],
                );
            }

            if session.cloud.sync_enabled && session.cloud.share_mode != ShareMode::Local {
                // Build full artifact content with imports + all sibling nodes
                // so the cloud server can reverify the proof standalone.
                let mut full_content = String::new();
                let imports = if session.proof.imports.is_empty() {
                    vec!["Mathlib".to_string()]
                } else {
                    session.proof.imports.clone()
                };
                for imp in &imports {
                    full_content.push_str(&format!("import {imp}\n"));
                }
                full_content.push('\n');
                // Include sibling nodes first (dependencies)
                let mut seen_labels = std::collections::BTreeSet::new();
                for sibling in &session.proof.nodes {
                    if sibling.id == node.id || sibling.content.trim().is_empty() {
                        continue;
                    }
                    if !seen_labels.insert(sibling.label.clone()) {
                        continue;
                    }
                    let clean = sibling.content.lines()
                        .filter(|l| !l.trim().starts_with("import ") && !l.trim().starts_with("open "))
                        .collect::<Vec<_>>().join("\n");
                    if !clean.trim().is_empty() {
                        full_content.push_str(clean.trim());
                        full_content.push_str("\n\n");
                    }
                }
                // Add the active node
                let clean_node = node.content.lines()
                    .filter(|l| !l.trim().starts_with("import ") && !l.trim().starts_with("open "))
                    .collect::<Vec<_>>().join("\n");
                full_content.push_str(clean_node.trim());

                let payload = serde_json::json!({
                    "visibilityScope": visibility,
                    "items": [{
                        "identityKey": format!(
                            "user-verified/{}/{}/{}",
                            sanitize_identity_segment(session.id.as_str()),
                            sanitize_identity_segment(node.label.as_str()),
                            stable_hash(node.statement.as_str())
                        ),
                        "label": node.label,
                        "statement": node.statement,
                        "artifactId": artifact_id,
                        "artifactContent": full_content,
                        "visibility": visibility,
                    }]
                });
                tx.execute(
                    r#"
                    INSERT INTO sync_queue (id, session_id, queue_type, payload_json, status, created_at, updated_at)
                    VALUES (?, ?, 'corpus.contribute', ?, 'pending', ?, ?)
                    "#,
                    params![
                        next_store_id("sync"),
                        session.id,
                        serde_json::to_string(&payload)?,
                        now,
                        now,
                    ],
                )?;
            }
        } else {
            let failure_class = classify_failure(result);
            let attempt_hash = stable_hash(
                format!("{}::{}::{}", node.statement, node.content, failure_class).as_str(),
            );
            let diagnostic_summary = summarize_lean_diagnostic(result);
            let snippet_short: String = node.content.chars().take(200).collect();
            tx.execute(
                r#"
                INSERT INTO attempt_logs
                (id, attempt_hash, session_id, target_hash, target_label, target_statement, attempt_kind, target_node_id, failure_class, snippet, rendered_scratch, diagnostic, imports_json, metadata_json, occurrence_count, first_seen_at, last_seen_at)
                VALUES (?, ?, ?, ?, ?, ?, 'node', ?, ?, ?, ?, ?, ?, ?, 1, ?, ?)
                ON CONFLICT(attempt_hash) DO UPDATE SET
                    diagnostic = excluded.diagnostic,
                    rendered_scratch = excluded.rendered_scratch,
                    last_seen_at = excluded.last_seen_at,
                    occurrence_count = attempt_logs.occurrence_count + 1
                "#,
                params![
                    next_store_id("attempt"),
                    &attempt_hash,
                    session.id,
                    stable_hash(node.statement.as_str()),
                    node.label,
                    node.statement,
                    node.id,
                    &failure_class,
                    node.content,
                    result.rendered_scratch,
                    &diagnostic_summary,
                    serde_json::to_string(&session.proof.imports)?,
                    serde_json::to_string(&serde_json::json!({
                        "workspaceRoot": session.workspace_root,
                        "workspaceLabel": session.workspace_label
                    }))?,
                    now,
                    now,
                ],
            )?;

            // Queue failure for cloud sync so future sessions learn from it
            let failure_payload = serde_json::json!({
                "attempts": [{
                    "attempt_hash": &attempt_hash,
                    "target_label": node.label,
                    "target_statement": node.statement,
                    "failure_class": &failure_class,
                    "snippet": snippet_short,
                    "diagnostic": &diagnostic_summary,
                }]
            });
            tx.execute(
                r#"
                INSERT INTO sync_queue (id, session_id, queue_type, payload_json, status, created_at, updated_at)
                VALUES (?, ?, 'attempt.failure', ?, 'pending', ?, ?)
                "#,
                params![
                    next_store_id("sync"),
                    session.id,
                    serde_json::to_string(&failure_payload)?,
                    now,
                    now,
                ],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    pub fn get_corpus_summary(&self) -> Result<CorpusSummary> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT
                (SELECT COUNT(*) FROM verified_corpus_items) AS local_entry_count,
                (SELECT COUNT(*) FROM verified_corpus_items) AS verified_entry_count,
                (SELECT COUNT(*) FROM verified_corpus_clusters) AS cluster_count,
                (SELECT COUNT(*) FROM verified_corpus_items WHERE cluster_role = 'member') AS duplicate_member_count,
                (SELECT COUNT(*) FROM attempt_logs) AS attempt_log_count,
                (SELECT COUNT(*) FROM verified_corpus_items WHERE is_library_seed = 1) AS library_seed_count,
                (SELECT COUNT(*) FROM verified_corpus_items WHERE origin = 'user-verified') AS user_verified_count,
                (SELECT MAX(updated_at) FROM verified_corpus_items) AS latest_updated_at
            "#,
        )?;
        Ok(stmt.query_row([], |row| {
            Ok(CorpusSummary {
                local_entry_count: row.get::<_, i64>(0)?.max(0) as usize,
                verified_entry_count: row.get::<_, i64>(1)?.max(0) as usize,
                cluster_count: row.get::<_, i64>(2)?.max(0) as usize,
                duplicate_member_count: row.get::<_, i64>(3)?.max(0) as usize,
                attempt_log_count: row.get::<_, i64>(4)?.max(0) as usize,
                library_seed_count: row.get::<_, i64>(5)?.max(0) as usize,
                user_verified_count: row.get::<_, i64>(6)?.max(0) as usize,
                latest_updated_at: row.get(7)?,
            })
        })?)
    }

    pub fn rebuild_verified_corpus_clusters(&self) -> Result<CorpusSummary> {
        let conn = self.connect()?;
        let tx = conn.unchecked_transaction()?;
        let mut stmt = tx.prepare(
            r#"
            SELECT id, label, statement, decl_kind, is_theorem_like, content_hash, created_at, updated_at
            FROM verified_corpus_items
            ORDER BY created_at ASC, id ASC
            "#,
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
            ))
        })?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        drop(stmt);

        tx.execute("DELETE FROM verified_corpus_clusters", [])?;
        tx.execute(
            "UPDATE verified_corpus_items SET cluster_id = NULL, cluster_role = NULL, equivalence_confidence = 1",
            [],
        )?;

        let mut clusters: BTreeMap<String, CorpusClusterRecord> = BTreeMap::new();
        let update_item = tx.prepare(
            r#"
            UPDATE verified_corpus_items
            SET cluster_id = ?, cluster_role = ?, equivalence_confidence = ?
            WHERE id = ?
            "#,
        )?;
        let insert_cluster = tx.prepare(
            r#"
            INSERT INTO verified_corpus_clusters
            (id, cluster_key, canonical_item_id, label, statement_preview, member_count, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )?;
        let mut update_item = update_item;
        let mut insert_cluster = insert_cluster;

        for (id, label, statement, decl_kind, is_theorem_like, content_hash, created_at, updated_at) in
            items
        {
            let cluster_key = compute_corpus_cluster_key(
                &statement,
                &decl_kind,
                is_theorem_like != 0,
                &content_hash,
            );
            let cluster =
                clusters
                    .entry(cluster_key.clone())
                    .or_insert_with(|| CorpusClusterRecord {
                        id: format!("cluster_{}", &cluster_key[..cluster_key.len().min(24)]),
                        cluster_key: cluster_key.clone(),
                        canonical_item_id: id.clone(),
                        label: if label.trim().is_empty() {
                            "cluster".to_string()
                        } else {
                            label.clone()
                        },
                        statement_preview: preview_text(&statement, 512),
                        member_count: 0,
                        created_at: created_at.clone(),
                        updated_at: updated_at.clone(),
                    });
            cluster.member_count += 1;
            if updated_at > cluster.updated_at {
                cluster.updated_at = updated_at.clone();
            }
            let role = if cluster.canonical_item_id == id {
                "canonical"
            } else {
                "member"
            };
            update_item.execute(params![
                cluster.id,
                role,
                if role == "canonical" { 1.0 } else { 0.92 },
                id
            ])?;
        }

        for cluster in clusters.values() {
            insert_cluster.execute(params![
                cluster.id,
                cluster.cluster_key,
                cluster.canonical_item_id,
                cluster.label,
                cluster.statement_preview,
                cluster.member_count as i64,
                cluster.created_at,
                cluster.updated_at
            ])?;
        }

        drop(update_item);
        drop(insert_cluster);
        tx.commit()?;
        self.get_corpus_summary()
    }

    pub fn search_verified_corpus(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<(String, String, String)>> {
        let conn = self.connect()?;

        // Extract keywords (3+ chars, skip common words) and search with AND logic.
        let skip = ["the", "that", "this", "with", "from", "prove", "show", "for", "and", "not"];
        let keywords: Vec<String> = query
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|w| w.len() >= 3 && !skip.contains(&w.to_lowercase().as_str()))
            .map(|w| format!("%{}%", w))
            .collect();

        if keywords.is_empty() {
            return Ok(Vec::new());
        }

        // Build WHERE clause: search_text LIKE %kw1% AND search_text LIKE %kw2% ...
        let conditions: Vec<String> = keywords.iter()
            .map(|_| "search_text LIKE ?".to_string())
            .collect();
        let where_clause = conditions.join(" AND ");
        let sql = format!(
            "SELECT label, statement, visibility FROM verified_corpus_items WHERE {} ORDER BY updated_at DESC LIMIT ?",
            where_clause
        );

        let mut stmt = conn.prepare(&sql)?;
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = keywords
            .iter()
            .map(|k| Box::new(k.clone()) as Box<dyn rusqlite::types::ToSql>)
            .collect();
        params.push(Box::new(limit as i64));

        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())), |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
        let mut items = Vec::new();
        let mut seen_labels = std::collections::HashSet::new();
        for row in rows {
            let item: (String, String, String) = row?;
            seen_labels.insert(item.0.clone());
            items.push(item);
        }

        // Graph expansion: find premises that the direct hits depend on.
        if !items.is_empty() {
            let direct_labels: Vec<String> = items.iter().map(|i| i.0.clone()).collect();
            let placeholders = direct_labels.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let edge_sql = format!(
                r#"SELECT DISTINCT v.label, v.statement, v.visibility
                FROM corpus_edges e
                JOIN verified_corpus_items v ON v.identity_key = e.to_item_key
                JOIN verified_corpus_items f ON f.identity_key = e.from_item_key
                WHERE f.label IN ({})
                LIMIT ?"#,
                placeholders
            );
            if let Ok(mut edge_stmt) = conn.prepare(&edge_sql) {
                let mut edge_params: Vec<Box<dyn rusqlite::types::ToSql>> = direct_labels
                    .iter()
                    .map(|l| Box::new(l.clone()) as Box<dyn rusqlite::types::ToSql>)
                    .collect();
                edge_params.push(Box::new(limit as i64));
                if let Ok(edge_rows) = edge_stmt.query_map(
                    rusqlite::params_from_iter(edge_params.iter().map(|p| p.as_ref())),
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)),
                ) {
                    for row in edge_rows {
                        if let Ok(item) = row {
                            if seen_labels.insert(item.0.clone()) {
                                items.push(item);
                            }
                        }
                    }
                }
            }
        }

        Ok(items)
    }

    /// List all user-verified items with their artifact content.
    /// Used to build the OpenProof.Corpus Lean module from local data.
    pub fn list_user_verified_with_artifacts(&self) -> Result<Vec<(String, String, String)>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            r#"SELECT v.label, v.statement, a.content
               FROM verified_corpus_items v
               JOIN verified_artifacts a ON v.artifact_id = a.id
               WHERE v.origin = 'user-verified'
               ORDER BY v.created_at ASC"#,
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
        let mut items = Vec::new();
        for row in rows {
            if let Ok(item) = row {
                items.push(item);
            }
        }
        Ok(items)
    }

    /// Get the full artifact content (proof code) for a corpus item by label.
    /// Returns None if not found or if it's a library-seed item without stored artifact.
    pub fn get_artifact_content(&self, label: &str) -> Result<Option<String>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            r#"SELECT a.content FROM verified_artifacts a
               JOIN verified_corpus_items v ON v.artifact_id = a.id
               WHERE v.label = ? AND v.origin = 'user-verified'
               LIMIT 1"#,
        )?;
        let result = stmt.query_row(rusqlite::params![label], |row| row.get(0)).ok();
        Ok(result)
    }
}
