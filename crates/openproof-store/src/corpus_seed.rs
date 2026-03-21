use anyhow::{Context, Result};
use chrono::Utc;
use std::fs;
use std::path::{Path, PathBuf};

use crate::corpus::{next_store_id, sanitize_identity_segment, stable_hash};
use crate::store::AppStore;

// ---------------------------------------------------------------------------
// Library seed item type and Lean file parsing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct LibrarySeedItem {
    pub kind: String,
    pub decl_name: String,
    pub statement: String,
    pub doc_string: Option<String>,
}

impl LibrarySeedItem {
    pub fn search_text(&self, module_name: &str, package_name: &str) -> String {
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

pub(crate) fn collect_lean_files(root: &Path) -> Result<Vec<PathBuf>> {
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

pub(crate) fn lean_module_name(relative_path: &Path) -> String {
    let without_extension = relative_path.with_extension("");
    without_extension
        .iter()
        .filter_map(|component| component.to_str())
        .collect::<Vec<_>>()
        .join(".")
}

pub(crate) fn extract_library_seed_items(source: &str) -> Vec<LibrarySeedItem> {
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
                if next.is_empty()
                    || parse_decl_header(next).is_some()
                    || next.starts_with("/--")
                {
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
    while matches!(
        first,
        "private" | "protected" | "noncomputable" | "unsafe" | "partial"
    ) {
        first = tokens.next()?;
    }
    let kind = match first {
        "theorem" | "lemma" | "def" | "instance" | "class" | "structure" | "inductive"
        | "abbrev" => first,
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

// ---------------------------------------------------------------------------
// AppStore impl: library seed ingestion and ingestion run tracking
// ---------------------------------------------------------------------------

impl AppStore {
    pub fn ingest_default_library_seeds(&self, lean_root: &Path) -> Result<Vec<(String, usize)>> {
        let mut results = Vec::new();
        let mathlib_root = lean_root
            .join(".lake")
            .join("packages")
            .join("mathlib")
            .join("Mathlib");
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
            let relative = file.strip_prefix(source_root).with_context(|| {
                format!(
                    "stripping {} from {}",
                    source_root.display(),
                    file.display()
                )
            })?;
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
                    rusqlite::params![
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
                    rusqlite::params![
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
                    rusqlite::params![
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
                        if matches!(item.kind.as_str(), "theorem" | "lemma") {
                            1
                        } else {
                            0
                        },
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
            rusqlite::params![id, kind, fingerprint, revision_hash, now, now],
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
            rusqlite::params![status, serde_json::to_string(stats)?, error, &now, &now, run_id],
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
            rusqlite::params![fingerprint, revision_hash],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    #[allow(clippy::too_many_arguments)]
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
            rusqlite::params![
                artifact_id,
                artifact_hash,
                decl_name,
                artifact_content,
                serde_json::to_string(metadata)?,
                &now,
                &now,
            ],
        )?;

        tx.execute(
            r#"
            INSERT OR IGNORE INTO verification_runs
            (id, session_id, target_kind, target_id, target_label, target_node_id, artifact_id, ok, code, stdout, stderr, error, scratch_path, rendered_scratch, created_at)
            VALUES (?, NULL, 'library_seed', ?, ?, NULL, ?, 1, NULL, '', '', NULL, '', ?, ?)
            "#,
            rusqlite::params![
                verification_run_id,
                identity_key,
                decl_name,
                artifact_id,
                statement,
                &now,
            ],
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
            rusqlite::params![
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
}
