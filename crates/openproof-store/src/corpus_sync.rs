use anyhow::Result;
use chrono::Utc;
use rusqlite::params;

use crate::corpus::stable_hash;
use crate::store::AppStore;

// ---------------------------------------------------------------------------
// AppStore impl: sync queue, remote cache, package/module upserts
// ---------------------------------------------------------------------------

impl AppStore {
    pub fn get_sync_summary(&self) -> Result<openproof_protocol::SyncSummary> {
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
            Ok(openproof_protocol::SyncSummary {
                pending_count: row.get::<_, Option<i64>>(0)?.unwrap_or(0).max(0) as usize,
                failed_count: row.get::<_, Option<i64>>(1)?.unwrap_or(0).max(0) as usize,
                sent_count: row.get::<_, Option<i64>>(2)?.unwrap_or(0).max(0) as usize,
            })
        })?)
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

    pub fn list_sync_jobs_full(
        &self,
        limit: usize,
    ) -> Result<Vec<openproof_protocol::SyncQueueItem>> {
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

    pub fn cache_remote_corpus_hits(
        &self,
        cache_key: &str,
        hits: &[openproof_protocol::CloudCorpusSearchHit],
    ) -> Result<()> {
        let conn = self.connect()?;
        let now = Utc::now().to_rfc3339();
        for hit in hits {
            let id = format!(
                "rcache_{}_{}",
                stable_hash(cache_key),
                stable_hash(&hit.identity_key)
            );
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
                    id,
                    cache_key,
                    hit.identity_key,
                    serde_json::to_string(hit)?,
                    hit.score,
                    &now,
                    &now
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
            if let Ok(hit) =
                serde_json::from_str::<openproof_protocol::CloudCorpusSearchHit>(&json_str)
            {
                results.push(hit);
            }
        }
        Ok(results)
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
                id,
                package_name,
                package_revision,
                source_type,
                source_url,
                serde_json::to_string(manifest)?,
                serde_json::to_string(root_modules)?,
                now,
                now
            ],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
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
                id,
                module_name,
                package_name,
                package_revision,
                source_path,
                serde_json::to_string(imports)?,
                environment_fingerprint,
                declaration_count as i64,
                now,
                now
            ],
        )?;
        Ok(())
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
        let placeholders = identity_keys
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
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
        let refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|b| b.as_ref()).collect();

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
                imports: serde_json::from_str::<Vec<String>>(
                    &row.get::<_, String>(21).unwrap_or_default(),
                )
                .unwrap_or_default(),
                metadata: serde_json::from_str::<serde_json::Value>(
                    &row.get::<_, String>(22).unwrap_or_default(),
                )
                .unwrap_or_default(),
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

    pub fn failed_attempts_for_target(
        &self,
        target_label: &str,
        limit: usize,
    ) -> Result<Vec<(String, String, String)>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT failure_class, snippet, diagnostic
            FROM attempt_logs
            WHERE target_label = ? OR target_statement LIKE ?
            ORDER BY last_seen_at DESC
            LIMIT ?
            "#,
        )?;
        let pattern = format!("%{}%", target_label);
        let rows = stmt.query_map(params![target_label, pattern, limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)
                    .unwrap_or_default()
                    .chars()
                    .take(200)
                    .collect::<String>(),
                row.get::<_, String>(2)
                    .unwrap_or_default()
                    .chars()
                    .take(200)
                    .collect::<String>(),
            ))
        })?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }
}
