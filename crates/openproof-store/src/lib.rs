use anyhow::{Context, Result};
use chrono::Utc;
use directories::BaseDirs;
use openproof_protocol::{
    CloudPolicy, CorpusSummary, LegacyImportSummary, LeanVerificationSummary, MessageRole,
    ProofNode, ProofNodeKind, ProofNodeStatus, ProofQuestionOption, ProofQuestionState,
    ProofSessionState, SessionSnapshot, ShareMode, SyncSummary, TranscriptEntry,
};
pub use rusqlite;
use rusqlite::{params, Connection};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct StorePaths {
    pub db_path: PathBuf,
    pub legacy_sessions_dir: PathBuf,
}

impl StorePaths {
    pub fn detect() -> Result<Self> {
        let base_dirs = BaseDirs::new().context("could not resolve home directory")?;
        let home = base_dirs.home_dir().join(".openproof");
        Ok(Self {
            db_path: home.join("native").join("openproof-native.sqlite"),
            legacy_sessions_dir: home.join("sessions"),
        })
    }
}

#[derive(Debug, Clone)]
pub struct AppStore {
    paths: StorePaths,
}

impl AppStore {
    pub fn open(paths: StorePaths) -> Result<Self> {
        if let Some(parent) = paths.db_path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let store = Self { paths };
        store.init_schema()?;
        Ok(store)
    }

    pub fn paths(&self) -> &StorePaths {
        &self.paths
    }

    pub fn db_path(&self) -> &Path {
        &self.paths.db_path
    }

    fn connect(&self) -> Result<Connection> {
        let conn = Connection::open(&self.paths.db_path)
            .with_context(|| format!("opening {}", self.paths.db_path.display()))?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA busy_timeout = 5000;
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                workspace_root TEXT,
                workspace_label TEXT,
                transcript_json TEXT NOT NULL,
                cloud_json TEXT NOT NULL DEFAULT '{}',
                proof_json TEXT NOT NULL DEFAULT '{}'
            );
            CREATE INDEX IF NOT EXISTS idx_sessions_updated_at ON sessions(updated_at DESC);
            CREATE TABLE IF NOT EXISTS verified_artifacts (
                id TEXT PRIMARY KEY,
                artifact_hash TEXT NOT NULL UNIQUE,
                label TEXT NOT NULL,
                content TEXT NOT NULL,
                imports_json TEXT NOT NULL DEFAULT '[]',
                namespace TEXT,
                metadata_json TEXT NOT NULL DEFAULT '{}',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS verification_runs (
                id TEXT PRIMARY KEY,
                session_id TEXT,
                target_kind TEXT NOT NULL,
                target_id TEXT,
                target_label TEXT,
                target_node_id TEXT,
                artifact_id TEXT,
                ok INTEGER NOT NULL,
                code INTEGER,
                stdout TEXT NOT NULL,
                stderr TEXT NOT NULL,
                error TEXT,
                scratch_path TEXT NOT NULL,
                rendered_scratch TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS verified_corpus_items (
                id TEXT PRIMARY KEY,
                statement_hash TEXT NOT NULL,
                identity_key TEXT NOT NULL UNIQUE,
                cluster_id TEXT,
                cluster_role TEXT,
                equivalence_confidence REAL NOT NULL DEFAULT 1,
                label TEXT NOT NULL,
                statement TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                artifact_id TEXT NOT NULL,
                verification_run_id TEXT NOT NULL,
                visibility TEXT NOT NULL,
                decl_name TEXT,
                module_name TEXT,
                package_name TEXT,
                package_revision TEXT,
                decl_kind TEXT NOT NULL,
                doc_string TEXT,
                search_text TEXT NOT NULL,
                origin TEXT NOT NULL,
                environment_fingerprint TEXT,
                is_theorem_like INTEGER NOT NULL DEFAULT 1,
                is_instance INTEGER NOT NULL DEFAULT 0,
                is_library_seed INTEGER NOT NULL DEFAULT 0,
                namespace TEXT,
                imports_json TEXT NOT NULL DEFAULT '[]',
                metadata_json TEXT NOT NULL DEFAULT '{}',
                source_session_id TEXT,
                source_node_id TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_verified_corpus_items_updated_at ON verified_corpus_items(updated_at DESC);
            CREATE INDEX IF NOT EXISTS idx_verified_corpus_items_visibility ON verified_corpus_items(visibility, updated_at DESC);
            CREATE TABLE IF NOT EXISTS verified_corpus_clusters (
                id TEXT PRIMARY KEY,
                cluster_key TEXT NOT NULL UNIQUE,
                canonical_item_id TEXT,
                label TEXT NOT NULL,
                statement_preview TEXT NOT NULL,
                member_count INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_verified_corpus_clusters_key ON verified_corpus_clusters(cluster_key);
            CREATE TABLE IF NOT EXISTS attempt_logs (
                id TEXT PRIMARY KEY,
                attempt_hash TEXT NOT NULL UNIQUE,
                session_id TEXT,
                target_hash TEXT NOT NULL,
                target_label TEXT NOT NULL,
                target_statement TEXT NOT NULL,
                attempt_kind TEXT NOT NULL,
                target_node_id TEXT,
                failure_class TEXT NOT NULL,
                snippet TEXT NOT NULL,
                rendered_scratch TEXT NOT NULL,
                diagnostic TEXT NOT NULL,
                imports_json TEXT NOT NULL DEFAULT '[]',
                metadata_json TEXT NOT NULL DEFAULT '{}',
                occurrence_count INTEGER NOT NULL DEFAULT 1,
                first_seen_at TEXT NOT NULL,
                last_seen_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_attempt_logs_target ON attempt_logs(target_hash, last_seen_at DESC);
            CREATE TABLE IF NOT EXISTS sync_queue (
                id TEXT PRIMARY KEY,
                session_id TEXT,
                queue_type TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_sync_queue_status ON sync_queue(status, updated_at DESC);
            "#,
        )?;
        ensure_column(
            &conn,
            "sessions",
            "cloud_json",
            "ALTER TABLE sessions ADD COLUMN cloud_json TEXT NOT NULL DEFAULT '{}'",
        )?;
        ensure_column(
            &conn,
            "sessions",
            "proof_json",
            "ALTER TABLE sessions ADD COLUMN proof_json TEXT NOT NULL DEFAULT '{}'",
        )?;
        ensure_column(
            &conn,
            "verified_corpus_items",
            "cluster_id",
            "ALTER TABLE verified_corpus_items ADD COLUMN cluster_id TEXT",
        )?;
        ensure_column(
            &conn,
            "verified_corpus_items",
            "cluster_role",
            "ALTER TABLE verified_corpus_items ADD COLUMN cluster_role TEXT",
        )?;
        ensure_column(
            &conn,
            "verified_corpus_items",
            "equivalence_confidence",
            "ALTER TABLE verified_corpus_items ADD COLUMN equivalence_confidence REAL NOT NULL DEFAULT 1",
        )?;
        conn.execute_batch(
            r#"
            CREATE INDEX IF NOT EXISTS idx_verified_corpus_items_cluster ON verified_corpus_items(cluster_id, updated_at DESC);
            CREATE INDEX IF NOT EXISTS idx_verified_corpus_clusters_key ON verified_corpus_clusters(cluster_key);

            CREATE TABLE IF NOT EXISTS corpus_packages (
                id TEXT PRIMARY KEY,
                package_name TEXT NOT NULL UNIQUE,
                package_revision TEXT,
                source_type TEXT NOT NULL,
                source_url TEXT,
                manifest_json TEXT NOT NULL DEFAULT '{}',
                root_modules_json TEXT NOT NULL DEFAULT '[]',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS corpus_modules (
                id TEXT PRIMARY KEY,
                module_name TEXT NOT NULL UNIQUE,
                package_name TEXT NOT NULL,
                package_revision TEXT,
                source_path TEXT,
                imports_json TEXT NOT NULL DEFAULT '[]',
                environment_fingerprint TEXT,
                declaration_count INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_corpus_modules_package ON corpus_modules(package_name, module_name);

            CREATE TABLE IF NOT EXISTS ingestion_runs (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                environment_fingerprint TEXT NOT NULL,
                package_revision_set_hash TEXT NOT NULL,
                status TEXT NOT NULL,
                stats_json TEXT NOT NULL DEFAULT '{}',
                error TEXT,
                started_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                completed_at TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_ingestion_runs_kind ON ingestion_runs(kind, updated_at DESC);

            CREATE TABLE IF NOT EXISTS remote_corpus_cache (
                id TEXT PRIMARY KEY,
                cache_key TEXT NOT NULL,
                identity_key TEXT NOT NULL,
                item_json TEXT NOT NULL,
                score REAL NOT NULL DEFAULT 0,
                cached_at TEXT NOT NULL,
                last_seen_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_remote_corpus_cache_key ON remote_corpus_cache(cache_key, last_seen_at DESC);
            "#,
        )?;
        Ok(conn)
    }

    fn init_schema(&self) -> Result<()> {
        let _ = self.connect()?;
        Ok(())
    }

    pub fn import_legacy_sessions(&self) -> Result<LegacyImportSummary> {
        let mut summary = LegacyImportSummary::default();
        if !self.paths.legacy_sessions_dir.exists() {
            self.ensure_default_session()?;
            return Ok(summary);
        }

        let entries = fs::read_dir(&self.paths.legacy_sessions_dir)
            .with_context(|| format!("reading {}", self.paths.legacy_sessions_dir.display()))?;
        for entry in entries {
            let entry = match entry {
                Ok(value) => value,
                Err(_) => {
                    summary.failed += 1;
                    continue;
                }
            };
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                summary.skipped += 1;
                continue;
            }
            match self.import_legacy_session_file(&path) {
                Ok(imported) => {
                    if imported {
                        summary.imported += 1;
                    } else {
                        summary.skipped += 1;
                    }
                }
                Err(_) => summary.failed += 1,
            }
        }
        self.ensure_default_session()?;
        Ok(summary)
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionSnapshot>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, title, updated_at, workspace_root, workspace_label, transcript_json, cloud_json, proof_json
            FROM sessions
            ORDER BY updated_at DESC
            "#,
        )?;
        let rows = stmt.query_map([], |row| {
            let transcript_json: String = row.get(5)?;
            let transcript =
                serde_json::from_str::<Vec<TranscriptEntry>>(&transcript_json).unwrap_or_default();
            let cloud_json: String = row.get(6)?;
            let cloud =
                serde_json::from_str::<CloudPolicy>(&cloud_json).unwrap_or_default();
            let proof_json: String = row.get(7)?;
            let proof =
                serde_json::from_str::<ProofSessionState>(&proof_json).unwrap_or_else(|_| default_proof_state());
            Ok(SessionSnapshot {
                id: row.get(0)?,
                title: row.get(1)?,
                updated_at: row.get(2)?,
                workspace_root: row.get(3)?,
                workspace_label: row.get(4)?,
                cloud,
                transcript,
                proof,
            })
        })?;

        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(row?);
        }
        Ok(sessions)
    }

    pub fn session_count(&self) -> Result<usize> {
        let conn = self.connect()?;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))?;
        Ok(count.max(0) as usize)
    }

    pub fn latest_session(&self) -> Result<Option<SessionSnapshot>> {
        Ok(self.list_sessions()?.into_iter().next())
    }

    pub fn get_session(&self, session_id: &str) -> Result<Option<SessionSnapshot>> {
        let conn = self.connect()?;
        self.session_by_id(&conn, session_id)
    }

    pub fn append_entry(&self, session_id: &str, entry: &TranscriptEntry) -> Result<()> {
        let conn = self.connect()?;
        let mut session = self
            .session_by_id(&conn, session_id)?
            .with_context(|| format!("missing session {session_id}"))?;
        session.updated_at = entry.created_at.clone();
        session.transcript.push(entry.clone());
        self.upsert_session(&conn, &session)
    }

    pub fn save_session(&self, session: &SessionSnapshot) -> Result<()> {
        let conn = self.connect()?;
        self.upsert_session(&conn, session)
    }

    pub fn record_verification_result(
        &self,
        session: &SessionSnapshot,
        result: &LeanVerificationSummary,
    ) -> Result<()> {
        let Some(active_node_id) = session.proof.active_node_id.as_deref() else {
            return Ok(());
        };
        let Some(node) = session.proof.nodes.iter().find(|node| node.id == active_node_id) else {
            return Ok(());
        };

        let conn = self.connect()?;
        let tx = conn.unchecked_transaction()?;
        let now = result.checked_at.clone();
        let content_hash = stable_hash(&node.content);
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
                node.content,
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

            if session.cloud.sync_enabled && session.cloud.share_mode != ShareMode::Local {
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
                        "artifactContent": node.content,
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
                    attempt_hash,
                    session.id,
                    stable_hash(node.statement.as_str()),
                    node.label,
                    node.statement,
                    node.id,
                    failure_class,
                    node.content,
                    result.rendered_scratch,
                    summarize_lean_diagnostic(result),
                    serde_json::to_string(&session.proof.imports)?,
                    serde_json::to_string(&serde_json::json!({
                        "workspaceRoot": session.workspace_root,
                        "workspaceLabel": session.workspace_label
                    }))?,
                    now,
                    now,
                ],
            )?;
        }

        tx.commit()?;
        if result.ok {
            let _ = self.rebuild_verified_corpus_clusters()?;
        }
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

        for (id, label, statement, decl_kind, is_theorem_like, content_hash, created_at, updated_at) in items {
            let cluster_key = compute_corpus_cluster_key(
                &statement,
                &decl_kind,
                is_theorem_like != 0,
                &content_hash,
            );
            let cluster = clusters.entry(cluster_key.clone()).or_insert_with(|| CorpusClusterRecord {
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
            update_item.execute(params![cluster.id, role, if role == "canonical" { 1.0 } else { 0.92 }, id])?;
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

    pub fn get_sync_summary(&self) -> Result<SyncSummary> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT
                SUM(CASE WHEN status = 'pending' THEN 1 ELSE 0 END) AS pending_count,
                SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END) AS failed_count,
                SUM(CASE WHEN status = 'sent' THEN 1 ELSE 0 END) AS sent_count
            FROM sync_queue
            "#,
        )?;
        Ok(stmt.query_row([], |row| {
            Ok(SyncSummary {
                pending_count: row.get::<_, Option<i64>>(0)?.unwrap_or(0).max(0) as usize,
                failed_count: row.get::<_, Option<i64>>(1)?.unwrap_or(0).max(0) as usize,
                sent_count: row.get::<_, Option<i64>>(2)?.unwrap_or(0).max(0) as usize,
            })
        })?)
    }

    pub fn search_verified_corpus(&self, query: &str, limit: usize) -> Result<Vec<(String, String, String)>> {
        let conn = self.connect()?;
        let pattern = format!("%{}%", query.trim());
        let mut stmt = conn.prepare(
            r#"
            SELECT label, statement, visibility
            FROM verified_corpus_items
            WHERE search_text LIKE ?
            ORDER BY updated_at DESC
            LIMIT ?
            "#,
        )?;
        let rows = stmt.query_map(params![pattern, limit as i64], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        Ok(items)
    }

    pub fn ingest_default_library_seeds(&self, lean_root: &Path) -> Result<Vec<(String, usize)>> {
        let mut results = Vec::new();
        let mathlib_root = lean_root.join(".lake").join("packages").join("mathlib").join("Mathlib");
        if mathlib_root.exists() {
            let count = self.ingest_library_seed_package("mathlib", &mathlib_root, None)?;
            results.push(("mathlib".to_string(), count));
        }
        let openproof_root = lean_root.join("OpenProof");
        if openproof_root.exists() {
            let count = self.ingest_library_seed_package("openproof", &openproof_root, None)?;
            results.push(("openproof".to_string(), count));
        }
        if !results.is_empty() {
            let _ = self.rebuild_verified_corpus_clusters()?;
        }
        Ok(results)
    }

    pub fn pending_sync_jobs(&self) -> Result<Vec<(String, String)>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, payload_json
            FROM sync_queue
            WHERE status = 'pending'
            ORDER BY created_at ASC
            "#,
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let mut jobs = Vec::new();
        for row in rows {
            jobs.push(row?);
        }
        Ok(jobs)
    }

    pub fn mark_sync_job_status(&self, job_id: &str, status: &str) -> Result<()> {
        let conn = self.connect()?;
        conn.execute(
            "UPDATE sync_queue SET status = ?, updated_at = ? WHERE id = ?",
            params![status, Utc::now().to_rfc3339(), job_id],
        )?;
        Ok(())
    }

    fn ensure_default_session(&self) -> Result<()> {
        let conn = self.connect()?;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))?;
        if count > 0 {
            return Ok(());
        }
        let now = Utc::now().to_rfc3339();
        let session = SessionSnapshot {
            id: format!("rust_session_{}", Utc::now().timestamp_millis()),
            title: "OpenProof Rust Session".to_string(),
            updated_at: now,
            workspace_root: None,
            workspace_label: Some("openproof".to_string()),
            cloud: CloudPolicy::default(),
            transcript: Vec::new(),
            proof: default_proof_state(),
        };
        self.upsert_session(&conn, &session)
    }

    fn import_legacy_session_file(&self, path: &Path) -> Result<bool> {
        let raw =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let value: Value =
            serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
        let Some(id) = value.get("id").and_then(Value::as_str).map(str::to_string) else {
            return Ok(false);
        };
        let title = value
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| "Imported OpenProof Session".to_string());
        let updated_at = value
            .get("updatedAt")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| Utc::now().to_rfc3339());
        let workspace_root = value
            .get("workspace")
            .and_then(|item| item.get("root"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let workspace_label = value
            .get("workspace")
            .and_then(|item| item.get("label"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let transcript = extract_transcript(&value);
        let snapshot = SessionSnapshot {
            id,
            title,
            updated_at,
            workspace_root,
            workspace_label,
            cloud: extract_cloud_policy(&value),
            transcript,
            proof: extract_proof_state(&value),
        };

        let conn = self.connect()?;
        self.upsert_session(&conn, &snapshot)?;
        Ok(true)
    }

    fn session_by_id(
        &self,
        conn: &Connection,
        session_id: &str,
    ) -> Result<Option<SessionSnapshot>> {
        let mut stmt = conn.prepare(
            r#"
            SELECT id, title, updated_at, workspace_root, workspace_label, transcript_json, cloud_json, proof_json
            FROM sessions
            WHERE id = ?
            "#,
        )?;
        let mut rows = stmt.query(params![session_id])?;
        if let Some(row) = rows.next()? {
            let transcript_json: String = row.get(5)?;
            let transcript =
                serde_json::from_str::<Vec<TranscriptEntry>>(&transcript_json).unwrap_or_default();
            let cloud_json: String = row.get(6)?;
            let cloud =
                serde_json::from_str::<CloudPolicy>(&cloud_json).unwrap_or_default();
            let proof_json: String = row.get(7)?;
            let proof =
                serde_json::from_str::<ProofSessionState>(&proof_json).unwrap_or_else(|_| default_proof_state());
            return Ok(Some(SessionSnapshot {
                id: row.get(0)?,
                title: row.get(1)?,
                updated_at: row.get(2)?,
                workspace_root: row.get(3)?,
                workspace_label: row.get(4)?,
                cloud,
                transcript,
                proof,
            }));
        }
        Ok(None)
    }

    fn upsert_session(&self, conn: &Connection, session: &SessionSnapshot) -> Result<()> {
        let transcript_json = serde_json::to_string(&session.transcript)?;
        let cloud_json = serde_json::to_string(&session.cloud)?;
        let proof_json = serde_json::to_string(&session.proof)?;
        conn.execute(
            r#"
            INSERT INTO sessions (id, title, updated_at, workspace_root, workspace_label, transcript_json, cloud_json, proof_json)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                title = excluded.title,
                updated_at = excluded.updated_at,
                workspace_root = excluded.workspace_root,
                workspace_label = excluded.workspace_label,
                transcript_json = excluded.transcript_json,
                cloud_json = excluded.cloud_json,
                proof_json = excluded.proof_json
            "#,
            params![
                session.id,
                session.title,
                session.updated_at,
                session.workspace_root,
                session.workspace_label,
                transcript_json,
                cloud_json,
                proof_json
            ],
        )?;
        Ok(())
    }

    fn ingest_library_seed_package(
        &self,
        package_name: &str,
        source_root: &Path,
        package_revision: Option<&str>,
    ) -> Result<usize> {
        let files = collect_lean_files(source_root)?;
        if files.is_empty() {
            return Ok(0);
        }
        let conn = self.connect()?;
        let tx = conn.unchecked_transaction()?;
        let now = Utc::now().to_rfc3339();
        let mut inserted = 0usize;
        for file in files {
            let relative = file
                .strip_prefix(source_root)
                .with_context(|| format!("stripping {} from {}", source_root.display(), file.display()))?;
            let module_name = lean_module_name(relative);
            let contents = fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            for item in extract_library_seed_items(&contents) {
                let artifact_hash = stable_hash(&item.statement);
                let artifact_id = format!("seed_artifact_{artifact_hash}");
                let verification_run_id = format!("seed_verification_{artifact_hash}");
                let identity_key = format!(
                    "library-seed/{}/{}/{}",
                    sanitize_identity_segment(package_name),
                    sanitize_identity_segment(&module_name),
                    sanitize_identity_segment(&item.decl_name)
                );
                tx.execute(
                    r#"
                    INSERT INTO verified_artifacts
                    (id, artifact_hash, label, content, imports_json, namespace, metadata_json, created_at, updated_at)
                    VALUES (?, ?, ?, ?, '[]', NULL, ?, ?, ?)
                    ON CONFLICT(artifact_hash) DO UPDATE SET
                        label = excluded.label,
                        content = excluded.content,
                        metadata_json = excluded.metadata_json,
                        updated_at = excluded.updated_at
                    "#,
                    params![
                        artifact_id.as_str(),
                        artifact_hash.as_str(),
                        item.decl_name.as_str(),
                        item.statement.as_str(),
                        serde_json::to_string(&serde_json::json!({
                            "moduleName": &module_name,
                            "packageName": package_name,
                            "sourcePath": file.display().to_string(),
                            "docString": item.doc_string.clone(),
                        }))?,
                        now,
                        now,
                    ],
                )?;
                tx.execute(
                    r#"
                    INSERT OR IGNORE INTO verification_runs
                    (id, session_id, target_kind, target_id, target_label, target_node_id, artifact_id, ok, code, stdout, stderr, error, scratch_path, rendered_scratch, created_at)
                    VALUES (?, NULL, 'library_seed', ?, ?, NULL, ?, 1, NULL, '', '', NULL, ?, ?, ?)
                    "#,
                    params![
                        verification_run_id.as_str(),
                        identity_key.as_str(),
                        item.decl_name.as_str(),
                        artifact_id.as_str(),
                        file.display().to_string(),
                        item.statement.as_str(),
                        now,
                    ],
                )?;
                tx.execute(
                    r#"
                    INSERT INTO verified_corpus_items
                    (id, statement_hash, identity_key, label, statement, content_hash, artifact_id, verification_run_id, visibility, decl_name, module_name, package_name, package_revision, decl_kind, doc_string, search_text, origin, environment_fingerprint, is_theorem_like, is_instance, is_library_seed, namespace, imports_json, metadata_json, source_session_id, source_node_id, created_at, updated_at)
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'library-seed', ?, ?, ?, ?, ?, ?, ?, 'library-seed', NULL, ?, ?, 1, NULL, '[]', ?, NULL, NULL, ?, ?)
                    ON CONFLICT(identity_key) DO UPDATE SET
                        label = excluded.label,
                        statement = excluded.statement,
                        content_hash = excluded.content_hash,
                        artifact_id = excluded.artifact_id,
                        verification_run_id = excluded.verification_run_id,
                        decl_name = excluded.decl_name,
                        module_name = excluded.module_name,
                        package_name = excluded.package_name,
                        package_revision = excluded.package_revision,
                        decl_kind = excluded.decl_kind,
                        doc_string = excluded.doc_string,
                        search_text = excluded.search_text,
                        is_theorem_like = excluded.is_theorem_like,
                        is_instance = excluded.is_instance,
                        is_library_seed = excluded.is_library_seed,
                        metadata_json = excluded.metadata_json,
                        updated_at = excluded.updated_at
                    "#,
                    params![
                        next_store_id("corpus"),
                        stable_hash(&item.statement),
                        identity_key.as_str(),
                        item.decl_name.as_str(),
                        item.statement.as_str(),
                        artifact_hash.as_str(),
                        artifact_id.as_str(),
                        verification_run_id.as_str(),
                        item.decl_name.as_str(),
                        module_name.as_str(),
                        package_name,
                        package_revision,
                        item.kind.as_str(),
                        item.doc_string.clone(),
                        item.search_text(&module_name, package_name),
                        if matches!(item.kind.as_str(), "theorem" | "lemma") { 1 } else { 0 },
                        if item.kind == "instance" { 1 } else { 0 },
                        serde_json::to_string(&serde_json::json!({
                            "sourcePath": file.display().to_string(),
                            "packageName": package_name,
                            "moduleName": &module_name,
                        }))?,
                        now,
                        now,
                    ],
                )?;
                inserted = inserted.saturating_add(1);
            }
        }
        tx.commit()?;
        Ok(inserted)
    }

    // --- New methods for corpus manager support ---

    /// Open a raw connection for bulk operations (used by corpus crate).
    pub fn connect_for_bulk(&self) -> Result<Connection> {
        self.connect()
    }

    pub fn upsert_corpus_package(
        &self,
        package_name: &str,
        package_revision: Option<&str>,
        source_type: &str,
        source_url: Option<&str>,
        manifest: &serde_json::Value,
        root_modules: &[String],
    ) -> Result<()> {
        let conn = self.connect()?;
        let now = Utc::now().to_rfc3339();
        let id = format!("pkg_{}", stable_hash(package_name));
        conn.execute(
            r#"
            INSERT INTO corpus_packages
            (id, package_name, package_revision, source_type, source_url, manifest_json, root_modules_json, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(package_name) DO UPDATE SET
                package_revision = excluded.package_revision,
                source_type = excluded.source_type,
                source_url = excluded.source_url,
                manifest_json = excluded.manifest_json,
                root_modules_json = excluded.root_modules_json,
                updated_at = excluded.updated_at
            "#,
            params![
                id, package_name, package_revision, source_type, source_url,
                serde_json::to_string(manifest)?,
                serde_json::to_string(root_modules)?,
                now, now
            ],
        )?;
        Ok(())
    }

    pub fn upsert_corpus_module(
        &self,
        module_name: &str,
        package_name: &str,
        package_revision: Option<&str>,
        source_path: Option<&str>,
        imports: &[String],
        environment_fingerprint: &str,
        declaration_count: usize,
    ) -> Result<()> {
        let conn = self.connect()?;
        let now = Utc::now().to_rfc3339();
        let id = format!("mod_{}", stable_hash(module_name));
        conn.execute(
            r#"
            INSERT INTO corpus_modules
            (id, module_name, package_name, package_revision, source_path, imports_json, environment_fingerprint, declaration_count, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(module_name) DO UPDATE SET
                package_name = excluded.package_name,
                package_revision = excluded.package_revision,
                source_path = excluded.source_path,
                imports_json = excluded.imports_json,
                environment_fingerprint = excluded.environment_fingerprint,
                declaration_count = excluded.declaration_count,
                updated_at = excluded.updated_at
            "#,
            params![
                id, module_name, package_name, package_revision, source_path,
                serde_json::to_string(imports)?,
                environment_fingerprint, declaration_count as i64,
                now, now
            ],
        )?;
        Ok(())
    }

    pub fn start_ingestion_run(
        &self,
        kind: &str,
        fingerprint: &str,
        revision_hash: &str,
    ) -> Result<String> {
        let conn = self.connect()?;
        let now = Utc::now().to_rfc3339();
        let id = next_store_id("ingest");
        conn.execute(
            r#"
            INSERT INTO ingestion_runs (id, kind, environment_fingerprint, package_revision_set_hash, status, stats_json, error, started_at, updated_at)
            VALUES (?, ?, ?, ?, 'running', '{}', NULL, ?, ?)
            "#,
            params![id, kind, fingerprint, revision_hash, now, now],
        )?;
        Ok(id)
    }

    pub fn finish_ingestion_run(
        &self,
        run_id: &str,
        status: &str,
        stats: &serde_json::Value,
        error: Option<&str>,
    ) -> Result<()> {
        let conn = self.connect()?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            r#"
            UPDATE ingestion_runs
            SET status = ?, stats_json = ?, error = ?, updated_at = ?, completed_at = ?
            WHERE id = ?
            "#,
            params![status, serde_json::to_string(stats)?, error, &now, &now, run_id],
        )?;
        Ok(())
    }

    pub fn has_completed_library_seed(
        &self,
        fingerprint: &str,
        revision_hash: &str,
    ) -> Result<bool> {
        let conn = self.connect()?;
        let count: i64 = conn.query_row(
            r#"
            SELECT COUNT(*) FROM ingestion_runs
            WHERE kind = 'library-seed'
              AND environment_fingerprint = ?
              AND package_revision_set_hash = ?
              AND status = 'completed'
            "#,
            params![fingerprint, revision_hash],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn upsert_library_seed_declaration_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        identity_key: &str,
        decl_name: &str,
        statement: &str,
        artifact_content: &str,
        verification_run_id: &str,
        module_name: &str,
        package_name: &str,
        package_revision: Option<&str>,
        decl_kind: &str,
        doc_string: Option<&str>,
        namespace: Option<&str>,
        search_text: &str,
        is_theorem_like: bool,
        is_instance: bool,
        fingerprint: &str,
        metadata: &serde_json::Value,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let artifact_hash = stable_hash(statement);
        let artifact_id = format!("seed_artifact_{artifact_hash}");

        tx.execute(
            r#"
            INSERT INTO verified_artifacts
            (id, artifact_hash, label, content, imports_json, namespace, metadata_json, created_at, updated_at)
            VALUES (?, ?, ?, ?, '[]', NULL, ?, ?, ?)
            ON CONFLICT(artifact_hash) DO UPDATE SET
                label = excluded.label,
                content = excluded.content,
                metadata_json = excluded.metadata_json,
                updated_at = excluded.updated_at
            "#,
            params![
                artifact_id, artifact_hash, decl_name, artifact_content,
                serde_json::to_string(metadata)?, &now, &now,
            ],
        )?;

        tx.execute(
            r#"
            INSERT OR IGNORE INTO verification_runs
            (id, session_id, target_kind, target_id, target_label, target_node_id, artifact_id, ok, code, stdout, stderr, error, scratch_path, rendered_scratch, created_at)
            VALUES (?, NULL, 'library_seed', ?, ?, NULL, ?, 1, NULL, '', '', NULL, '', ?, ?)
            "#,
            params![verification_run_id, identity_key, decl_name, artifact_id, statement, &now],
        )?;

        tx.execute(
            r#"
            INSERT INTO verified_corpus_items
            (id, statement_hash, identity_key, label, statement, content_hash, artifact_id, verification_run_id, visibility, decl_name, module_name, package_name, package_revision, decl_kind, doc_string, search_text, origin, environment_fingerprint, is_theorem_like, is_instance, is_library_seed, namespace, imports_json, metadata_json, source_session_id, source_node_id, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'library-seed', ?, ?, ?, ?, ?, ?, ?, 'library-seed', ?, ?, ?, 1, ?, '[]', ?, NULL, NULL, ?, ?)
            ON CONFLICT(identity_key) DO UPDATE SET
                label = excluded.label,
                statement = excluded.statement,
                content_hash = excluded.content_hash,
                artifact_id = excluded.artifact_id,
                verification_run_id = excluded.verification_run_id,
                decl_name = excluded.decl_name,
                module_name = excluded.module_name,
                package_name = excluded.package_name,
                package_revision = excluded.package_revision,
                decl_kind = excluded.decl_kind,
                doc_string = excluded.doc_string,
                search_text = excluded.search_text,
                is_theorem_like = excluded.is_theorem_like,
                is_instance = excluded.is_instance,
                is_library_seed = excluded.is_library_seed,
                metadata_json = excluded.metadata_json,
                updated_at = excluded.updated_at
            "#,
            params![
                next_store_id("corpus"),
                stable_hash(statement),
                identity_key,
                decl_name,
                statement,
                &artifact_hash,
                &artifact_id,
                verification_run_id,
                decl_name,
                module_name,
                package_name,
                package_revision,
                decl_kind,
                doc_string,
                search_text,
                fingerprint,
                if is_theorem_like { 1 } else { 0 },
                if is_instance { 1 } else { 0 },
                namespace,
                serde_json::to_string(metadata)?,
                &now,
                &now,
            ],
        )?;
        Ok(())
    }

    pub fn rebuild_corpus_search_index(&self) -> Result<()> {
        let conn = self.connect()?;
        conn.execute_batch(
            r#"
            DROP TABLE IF EXISTS verified_corpus_search;
            CREATE VIRTUAL TABLE IF NOT EXISTS verified_corpus_search USING fts5(
                item_id UNINDEXED,
                identity_key,
                label,
                statement,
                search_text,
                decl_name,
                module_name,
                package_name,
                doc_string,
                namespace,
                imports_text,
                tokenize = 'porter unicode61'
            );
            INSERT INTO verified_corpus_search
            (item_id, identity_key, label, statement, search_text, decl_name, module_name, package_name, doc_string, namespace, imports_text)
            SELECT
                id,
                identity_key,
                label,
                statement,
                search_text,
                COALESCE(decl_name, ''),
                COALESCE(module_name, ''),
                COALESCE(package_name, ''),
                COALESCE(doc_string, ''),
                COALESCE(namespace, ''),
                COALESCE(REPLACE(REPLACE(imports_json, '[', ''), ']', ''), '')
            FROM verified_corpus_items;
            "#,
        )?;
        Ok(())
    }

    pub fn cache_remote_corpus_hits(
        &self,
        cache_key: &str,
        hits: &[openproof_protocol::CloudCorpusSearchHit],
    ) -> Result<()> {
        let conn = self.connect()?;
        let now = Utc::now().to_rfc3339();
        for hit in hits {
            let id = format!("rcache_{}_{}", stable_hash(cache_key), stable_hash(&hit.identity_key));
            conn.execute(
                r#"
                INSERT INTO remote_corpus_cache (id, cache_key, identity_key, item_json, score, cached_at, last_seen_at)
                VALUES (?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(id) DO UPDATE SET
                    item_json = excluded.item_json,
                    score = excluded.score,
                    last_seen_at = excluded.last_seen_at
                "#,
                params![
                    id, cache_key, hit.identity_key,
                    serde_json::to_string(hit)?,
                    hit.score, &now, &now
                ],
            )?;
        }
        Ok(())
    }

    pub fn search_remote_corpus_cache(
        &self,
        query: &str,
        limit: usize,
        cache_key: &str,
    ) -> Result<Vec<openproof_protocol::CloudCorpusSearchHit>> {
        let conn = self.connect()?;
        let pattern = format!("%{}%", query.trim().to_lowercase());
        let mut stmt = conn.prepare(
            r#"
            SELECT item_json, score
            FROM remote_corpus_cache
            WHERE cache_key = ? AND LOWER(identity_key) LIKE ?
            ORDER BY score DESC, last_seen_at DESC
            LIMIT ?
            "#,
        )?;
        let rows = stmt.query_map(params![cache_key, pattern, limit as i64], |row| {
            let json_str: String = row.get(0)?;
            Ok(json_str)
        })?;
        let mut results = Vec::new();
        for row in rows {
            let json_str = row?;
            if let Ok(hit) = serde_json::from_str::<openproof_protocol::CloudCorpusSearchHit>(&json_str) {
                results.push(hit);
            }
        }
        Ok(results)
    }

    pub fn list_sync_jobs_full(&self, limit: usize) -> Result<Vec<openproof_protocol::SyncQueueItem>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, session_id, queue_type, payload_json, status, created_at, updated_at
            FROM sync_queue
            ORDER BY created_at ASC
            LIMIT ?
            "#,
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok(openproof_protocol::SyncQueueItem {
                id: row.get(0)?,
                session_id: row.get(1)?,
                queue_type: row.get(2)?,
                payload_json: row.get(3)?,
                status: row.get(4)?,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
            })
        })?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        Ok(items)
    }

    pub fn list_verified_upload_candidates(
        &self,
        limit: usize,
        identity_keys: &[String],
        visibility: &str,
    ) -> Result<Vec<openproof_protocol::CloudCorpusSearchHit>> {
        let conn = self.connect()?;
        let mut results = Vec::new();
        if identity_keys.is_empty() {
            return Ok(results);
        }
        let placeholders = identity_keys.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            r#"
            SELECT c.id, c.identity_key, c.label, c.statement, c.content_hash, c.artifact_id,
                   c.verification_run_id, c.visibility, c.decl_name, c.module_name,
                   c.package_name, c.package_revision, c.decl_kind, c.doc_string,
                   c.search_text, c.origin, c.environment_fingerprint,
                   c.is_theorem_like, c.is_instance, c.is_library_seed,
                   c.namespace, c.imports_json, c.metadata_json,
                   c.created_at, c.updated_at,
                   COALESCE(a.content, '') as artifact_content
            FROM verified_corpus_items c
            LEFT JOIN verified_artifacts a ON a.id = c.artifact_id
            WHERE c.identity_key IN ({placeholders})
              AND c.visibility = ?
              AND c.origin != 'library-seed'
            ORDER BY c.updated_at DESC
            LIMIT ?
            "#,
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = identity_keys
            .iter()
            .map(|k| Box::new(k.clone()) as Box<dyn rusqlite::types::ToSql>)
            .collect();
        param_values.push(Box::new(visibility.to_string()));
        param_values.push(Box::new(limit as i64));
        let refs: Vec<&dyn rusqlite::types::ToSql> = param_values.iter().map(|b| b.as_ref()).collect();

        let rows = stmt.query_map(refs.as_slice(), |row| {
            Ok(openproof_protocol::CloudCorpusSearchHit {
                id: row.get(0)?,
                identity_key: row.get(1)?,
                label: row.get(2)?,
                statement: row.get(3)?,
                content_hash: row.get(4)?,
                artifact_id: row.get(5)?,
                verification_run_id: row.get(6)?,
                visibility: row.get(7)?,
                decl_name: row.get(8)?,
                module_name: row.get(9)?,
                package_name: row.get(10)?,
                package_revision: row.get(11)?,
                decl_kind: row.get::<_, String>(12).unwrap_or_default(),
                doc_string: row.get(13)?,
                search_text: row.get::<_, String>(14).unwrap_or_default(),
                origin: row.get::<_, String>(15).unwrap_or_default(),
                environment_fingerprint: row.get(16)?,
                is_theorem_like: row.get::<_, i64>(17).unwrap_or(0) == 1,
                is_instance: row.get::<_, i64>(18).unwrap_or(0) == 1,
                is_library_seed: row.get::<_, i64>(19).unwrap_or(0) == 1,
                namespace: row.get(20)?,
                imports: serde_json::from_str::<Vec<String>>(&row.get::<_, String>(21).unwrap_or_default()).unwrap_or_default(),
                metadata: serde_json::from_str::<serde_json::Value>(&row.get::<_, String>(22).unwrap_or_default()).unwrap_or_default(),
                created_at: row.get::<_, String>(23).unwrap_or_default(),
                updated_at: row.get::<_, String>(24).unwrap_or_default(),
                artifact_content: row.get::<_, String>(25).unwrap_or_default(),
                score: 0.0,
                statement_hash: String::new(),
                cluster_id: None,
                cluster_role: None,
                equivalence_confidence: None,
                kind: String::new(),
                source_session_id: None,
                source_node_id: None,
            })
        })?;
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }
}

fn ensure_column(conn: &Connection, table: &str, column: &str, ddl: &str) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for existing in columns {
        if existing? == column {
            return Ok(());
        }
    }
    conn.execute_batch(ddl)?;
    Ok(())
}

#[derive(Debug, Clone)]
struct LibrarySeedItem {
    kind: String,
    decl_name: String,
    statement: String,
    doc_string: Option<String>,
}

impl LibrarySeedItem {
    fn search_text(&self, module_name: &str, package_name: &str) -> String {
        [
            self.decl_name.as_str(),
            self.statement.as_str(),
            module_name,
            package_name,
            self.doc_string.as_deref().unwrap_or(""),
        ]
        .join(" ")
    }
}

fn collect_lean_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        for entry in fs::read_dir(&path).with_context(|| format!("reading {}", path.display()))? {
            let entry = entry?;
            let entry_path = entry.path();
            if entry.file_type()?.is_dir() {
                stack.push(entry_path);
            } else if entry_path.extension().and_then(|ext| ext.to_str()) == Some("lean") {
                files.push(entry_path);
            }
        }
    }
    Ok(files)
}

fn lean_module_name(relative_path: &Path) -> String {
    let without_extension = relative_path.with_extension("");
    without_extension
        .iter()
        .filter_map(|component| component.to_str())
        .collect::<Vec<_>>()
        .join(".")
}

fn extract_library_seed_items(source: &str) -> Vec<LibrarySeedItem> {
    let mut items = Vec::new();
    let lines = source.lines().collect::<Vec<_>>();
    let mut index = 0usize;
    let mut pending_doc: Option<String> = None;
    while index < lines.len() {
        let trimmed = lines[index].trim();
        if trimmed.is_empty() {
            index += 1;
            continue;
        }
        if trimmed.starts_with("/--") {
            let mut doc = trimmed.trim_start_matches("/--").trim().to_string();
            while !doc.contains("-/") && index + 1 < lines.len() {
                index += 1;
                doc.push(' ');
                doc.push_str(lines[index].trim());
            }
            pending_doc = Some(doc.replace("-/", "").trim().to_string());
            index += 1;
            continue;
        }
        if let Some((kind, decl_name)) = parse_decl_header(trimmed) {
            let mut header = trimmed.to_string();
            while !header.contains(":=")
                && !header.contains(" where")
                && !header.ends_with("where")
                && index + 1 < lines.len()
            {
                let next = lines[index + 1].trim();
                if next.is_empty() || parse_decl_header(next).is_some() || next.starts_with("/--") {
                    break;
                }
                header.push(' ');
                header.push_str(next);
                index += 1;
                if header.contains(":=") || header.contains(" where") || header.ends_with("where") {
                    break;
                }
            }
            let statement = header
                .split(":=")
                .next()
                .unwrap_or(header.as_str())
                .trim()
                .to_string();
            if !decl_name.contains("._") && !decl_name.starts_with('_') {
                items.push(LibrarySeedItem {
                    kind,
                    decl_name,
                    statement,
                    doc_string: pending_doc.take(),
                });
            } else {
                pending_doc = None;
            }
        } else {
            pending_doc = None;
        }
        index += 1;
    }
    items
}

fn parse_decl_header(line: &str) -> Option<(String, String)> {
    let mut tokens = line.split_whitespace();
    let mut first = tokens.next()?;
    while matches!(first, "private" | "protected" | "noncomputable" | "unsafe" | "partial") {
        first = tokens.next()?;
    }
    let kind = match first {
        "theorem" | "lemma" | "def" | "instance" | "class" | "structure" | "inductive" | "abbrev" => first,
        _ => return None,
    };
    let raw_name = tokens.next()?.trim();
    let decl_name = raw_name
        .trim_matches(|ch: char| matches!(ch, '(' | '{' | '['))
        .trim_end_matches(':')
        .trim_end_matches(":=")
        .trim_end_matches(',')
        .trim()
        .to_string();
    if decl_name.is_empty() {
        None
    } else {
        Some((kind.to_string(), decl_name))
    }
}

fn default_proof_state() -> ProofSessionState {
    ProofSessionState {
        phase: "idle".to_string(),
        status_line: "Ready.".to_string(),
        root_node_id: None,
        problem: None,
        formal_target: None,
        accepted_target: None,
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
        goal_summary: None,
        latest_diagnostics: None,
        active_node_id: None,
        active_branch_id: None,
        active_agent_role: None,
        active_foreground_branch_id: None,
        resolved_by_branch_id: None,
        hidden_branch_count: 0,
        imports: vec!["Mathlib".to_string()],
        nodes: Vec::new(),
        branches: Vec::new(),
        agents: Vec::new(),
        last_rendered_scratch: None,
        last_verification: None,
        paper_tex: String::new(),
    }
}

fn extract_cloud_policy(value: &Value) -> CloudPolicy {
    let share_mode = match value
        .get("cloud")
        .and_then(|item| item.get("shareMode"))
        .and_then(Value::as_str)
        .unwrap_or("local")
    {
        "community" => ShareMode::Community,
        "private" => ShareMode::Private,
        _ => ShareMode::Local,
    };
    CloudPolicy {
        sync_enabled: value
            .get("cloud")
            .and_then(|item| item.get("syncEnabled"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        share_mode,
        private_overlay_community: value
            .get("cloud")
            .and_then(|item| item.get("privateOverlayCommunity"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        last_sync_at: value
            .get("cloud")
            .and_then(|item| item.get("lastSyncAt"))
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

fn extract_proof_state(value: &Value) -> ProofSessionState {
    let mut proof = default_proof_state();
    proof.problem = value
        .get("proof")
        .and_then(|item| item.get("intent"))
        .and_then(|item| item.get("problem"))
        .and_then(Value::as_str)
        .map(str::to_string);
    proof.formal_target = value
        .get("proof")
        .and_then(|item| item.get("intent"))
        .and_then(|item| item.get("formalTarget"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            value.get("proof")
                .and_then(|item| item.get("formalization"))
                .and_then(|item| item.get("provisionalTarget"))
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    proof.accepted_target = value
        .get("proof")
        .and_then(|item| item.get("intent"))
        .and_then(|item| item.get("acceptedTarget"))
        .and_then(Value::as_str)
        .map(str::to_string);
    proof.search_status = value
        .get("proof")
        .and_then(|item| item.get("lastSearchStatus"))
        .and_then(Value::as_str)
        .map(str::to_string);
    proof.assumptions = value
        .get("proof")
        .and_then(|item| item.get("formalization"))
        .and_then(|item| item.get("assumptions"))
        .and_then(extract_string_array)
        .unwrap_or_default();
    proof.paper_notes = value
        .get("paper")
        .and_then(|item| item.get("notes"))
        .and_then(Value::as_array)
        .map(|notes| {
            notes.iter()
                .filter_map(|note| note.get("text").and_then(Value::as_str).map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    proof.pending_question = extract_pending_question(value);
    proof.awaiting_clarification = value
        .get("proof")
        .and_then(|item| item.get("awaitingClarification"))
        .and_then(Value::as_bool)
        .unwrap_or(proof.pending_question.is_some());
    proof.is_autonomous_running = value
        .get("proof")
        .and_then(|item| item.get("isAutonomousRunning"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    proof.autonomous_iteration_count = value
        .get("proof")
        .and_then(|item| item.get("autonomousIterationCount"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    proof.autonomous_started_at = value
        .get("proof")
        .and_then(|item| item.get("autonomousStartedAt"))
        .and_then(Value::as_str)
        .map(str::to_string);
    proof.autonomous_last_progress_at = value
        .get("proof")
        .and_then(|item| item.get("autonomousLastProgressAt"))
        .and_then(Value::as_str)
        .map(str::to_string);
    proof.autonomous_pause_reason = value
        .get("proof")
        .and_then(|item| item.get("autonomousPauseReason"))
        .and_then(Value::as_str)
        .map(str::to_string);
    proof.autonomous_stop_reason = value
        .get("proof")
        .and_then(|item| item.get("autonomousStopReason"))
        .and_then(Value::as_str)
        .map(str::to_string);
    proof.hidden_best_branch_id = value
        .get("proof")
        .and_then(|item| item.get("hiddenBestBranchId"))
        .and_then(Value::as_str)
        .map(str::to_string);
    proof.active_retrieval_summary = value
        .get("proof")
        .and_then(|item| item.get("activeRetrievalSummary"))
        .and_then(Value::as_str)
        .map(str::to_string);
    proof.strategy_summary = value
        .get("proof")
        .and_then(|item| item.get("strategySummary"))
        .and_then(Value::as_str)
        .map(str::to_string);
    proof.goal_summary = value
        .get("proof")
        .and_then(|item| item.get("goalSummary"))
        .and_then(Value::as_str)
        .map(str::to_string);
    proof.latest_diagnostics = value
        .get("proof")
        .and_then(|item| item.get("latestDiagnostics"))
        .and_then(Value::as_str)
        .map(str::to_string);
    proof.phase = value
        .get("proof")
        .and_then(|item| item.get("phase"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| proof.phase.clone());
    proof.status_line = value
        .get("proof")
        .and_then(|item| item.get("statusLine"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| proof.status_line.clone());
    proof.active_node_id = value
        .get("activeNodeId")
        .and_then(Value::as_str)
        .map(str::to_string);
    proof.root_node_id = value
        .get("proof")
        .and_then(|item| item.get("rootNodeId"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| proof.active_node_id.clone());
    proof.nodes = extract_proof_nodes(value);
    proof.last_verification = extract_last_verification(value);
    if let Some(result) = &proof.last_verification {
        proof.last_rendered_scratch = Some(result.rendered_scratch.clone());
    }
    proof
}

fn extract_string_array(value: &Value) -> Option<Vec<String>> {
    value.as_array().map(|items| {
        items.iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>()
    })
}

fn extract_pending_question(value: &Value) -> Option<ProofQuestionState> {
    let raw = value.get("proof").and_then(|item| item.get("pendingQuestion"))?;
    let prompt = raw.get("prompt").and_then(Value::as_str)?.trim().to_string();
    if prompt.is_empty() {
        return None;
    }
    let options = raw
        .get("options")
        .and_then(Value::as_array)
        .map(|items| {
            items.iter()
                .filter_map(|item| {
                    let id = item.get("id").and_then(Value::as_str)?.trim().to_string();
                    if id.is_empty() {
                        return None;
                    }
                    Some(ProofQuestionOption {
                        id: id.clone(),
                        label: item
                            .get("label")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or(id),
                        summary: item
                            .get("summary")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_default(),
                        formal_target: item
                            .get("formalTarget")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_default(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Some(ProofQuestionState {
        prompt,
        options,
        recommended_option_id: raw
            .get("recommendedOptionId")
            .and_then(Value::as_str)
            .map(str::to_string),
        answer_text: raw
            .get("answerText")
            .and_then(Value::as_str)
            .map(str::to_string),
        status: raw
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| "open".to_string()),
    })
}

fn extract_proof_nodes(value: &Value) -> Vec<ProofNode> {
    let Some(nodes) = value.get("nodes").and_then(Value::as_array) else {
        return Vec::new();
    };
    nodes.iter()
        .filter_map(|node| {
            let kind = match node.get("kind").and_then(Value::as_str).unwrap_or("theorem") {
                "lemma" => ProofNodeKind::Lemma,
                "theorem" => ProofNodeKind::Theorem,
                "artifact" => ProofNodeKind::Artifact,
                "attempt" => ProofNodeKind::Attempt,
                "conjecture" => ProofNodeKind::Conjecture,
                _ => return None,
            };
            let label = node.get("label").and_then(Value::as_str)?.trim().to_string();
            let statement = node
                .get("statement")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if label.is_empty() || statement.is_empty() {
                return None;
            }
            let status = match node.get("status").and_then(Value::as_str).unwrap_or("pending") {
                "suggested" => ProofNodeStatus::Suggested,
                "proving" => ProofNodeStatus::Proving,
                "verifying" => ProofNodeStatus::Verifying,
                "verified" => ProofNodeStatus::Verified,
                "failed" => ProofNodeStatus::Failed,
                "abandoned" => ProofNodeStatus::Abandoned,
                _ => ProofNodeStatus::Pending,
            };
            Some(ProofNode {
                id: node
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("legacy_node_{}", label)),
                kind,
                label,
                statement,
                content: node
                    .get("content")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_default(),
                status,
                created_at: node
                    .get("createdAt")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| Utc::now().to_rfc3339()),
                updated_at: node
                    .get("updatedAt")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| Utc::now().to_rfc3339()),
            })
        })
        .collect()
}

fn extract_last_verification(value: &Value) -> Option<LeanVerificationSummary> {
    let raw = value.get("runtime").and_then(|item| item.get("lastLeanCheck"))?;
    Some(LeanVerificationSummary {
        ok: raw.get("ok").and_then(Value::as_bool).unwrap_or(false),
        code: raw.get("code").and_then(Value::as_i64).map(|value| value as i32),
        stdout: raw
            .get("stdout")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_default(),
        stderr: raw
            .get("stderr")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_default(),
        error: raw.get("error").and_then(Value::as_str).map(str::to_string),
        checked_at: raw
            .get("checkedAt")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| Utc::now().to_rfc3339()),
        project_dir: raw
            .get("projectDir")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_default(),
        scratch_path: raw
            .get("scratchPath")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_default(),
        rendered_scratch: raw
            .get("renderedScratch")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_default(),
    })
}

fn extract_transcript(value: &Value) -> Vec<TranscriptEntry> {
    let mut transcript = Vec::new();

    if let Some(messages) = value.get("messages").and_then(Value::as_array) {
        for message in messages {
            let role = match message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("notice")
            {
                "user" => MessageRole::User,
                "assistant" => MessageRole::Assistant,
                "system" => MessageRole::System,
                _ => MessageRole::Notice,
            };
            transcript.push(TranscriptEntry {
                id: message
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("msg_{}", transcript.len())),
                role,
                title: None,
                content: message
                    .get("content")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_default(),
                created_at: message
                    .get("createdAt")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| Utc::now().to_rfc3339()),
            });
        }
    }

    if let Some(events) = value.get("events").and_then(Value::as_array) {
        for event in events {
            let title = event
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_string);
            let detail = event
                .get("detail")
                .and_then(Value::as_str)
                .unwrap_or_default();
            transcript.push(TranscriptEntry {
                id: event
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("evt_{}", transcript.len())),
                role: MessageRole::Notice,
                title,
                content: detail.to_string(),
                created_at: event
                    .get("createdAt")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| Utc::now().to_rfc3339()),
            });
        }
    }

    transcript.sort_by(|left, right| left.created_at.cmp(&right.created_at));
    transcript
}

fn next_store_id(prefix: &str) -> String {
    format!("{prefix}_{}", Utc::now().timestamp_millis())
}

fn stable_hash(input: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn sanitize_identity_segment(input: &str) -> String {
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

fn share_mode_to_str(mode: ShareMode) -> &'static str {
    match mode {
        ShareMode::Local => "local",
        ShareMode::Community => "community",
        ShareMode::Private => "private",
    }
}

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
        if is_theorem_like { "theorem-like" } else { "declaration" },
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

fn summarize_lean_diagnostic(result: &LeanVerificationSummary) -> String {
    let primary = if !result.stderr.trim().is_empty() {
        result.stderr.trim()
    } else if !result.stdout.trim().is_empty() {
        result.stdout.trim()
    } else {
        result.error.as_deref().unwrap_or("Lean verification failed.")
    };
    primary.lines().take(12).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_paths(name: &str) -> StorePaths {
        let root = std::env::temp_dir().join(format!(
            "openproof-store-test-{}-{}",
            name,
            Utc::now().timestamp_millis()
        ));
        StorePaths {
            db_path: root.join("openproof-native.sqlite"),
            legacy_sessions_dir: root.join("legacy-sessions"),
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
        let items = extract_library_seed_items(source);
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
