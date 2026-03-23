use anyhow::Result;
use openproof_cloud::CloudCorpusClient;
use openproof_protocol::{
    CloudCorpusAuthContext, CloudCorpusSearchHit, CloudCorpusUploadItem, ShareMode,
};
use openproof_store::AppStore;
use std::collections::HashMap;

use crate::DrainSyncResult;

/// Search the shared corpus across local cache and remote, with deduplication.
pub async fn search_shared_corpus(
    store: &AppStore,
    cloud_client: &CloudCorpusClient,
    query: &str,
    limit: usize,
    share_mode: ShareMode,
    auth: Option<&CloudCorpusAuthContext>,
    include_community_overlay: bool,
) -> Result<Vec<CloudCorpusSearchHit>> {
    if share_mode == ShareMode::Local {
        return Ok(Vec::new());
    }
    if !cloud_client.is_configured() {
        return Ok(Vec::new());
    }

    let mut scopes: Vec<(ShareMode, Option<&CloudCorpusAuthContext>)> =
        vec![(share_mode, auth)];
    if share_mode == ShareMode::Private && include_community_overlay {
        scopes.push((ShareMode::Community, auth));
    }

    let mut all_hits = Vec::new();
    for (scope_mode, scope_auth) in &scopes {
        match cloud_client
            .search_verified_remote(query, limit, *scope_mode, *scope_auth)
            .await
        {
            Ok(hits) => {
                // Cache the results locally
                if let Some(cache_key) = cloud_client.cache_key(*scope_mode, *scope_auth) {
                    let store = store.clone();
                    let hits_clone = hits.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        store.cache_remote_corpus_hits(&cache_key, &hits_clone)
                    })
                    .await;
                }
                all_hits.extend(hits);
            }
            Err(_) => {
                // Fall back to cached results
                if let Some(cache_key) = cloud_client.cache_key(*scope_mode, *scope_auth) {
                    let store = store.clone();
                    let q = query.to_string();
                    let cached = tokio::task::spawn_blocking(move || {
                        store.search_remote_corpus_cache(&q, limit, &cache_key)
                    })
                    .await
                    .ok()
                    .and_then(|r| r.ok())
                    .unwrap_or_default();
                    all_hits.extend(cached);
                }
            }
        }
    }

    Ok(merge_remote_hits(
        all_hits,
        limit,
        share_mode,
        include_community_overlay,
    ))
}

fn merge_remote_hits(
    hits: Vec<CloudCorpusSearchHit>,
    limit: usize,
    share_mode: ShareMode,
    include_community_overlay: bool,
) -> Vec<CloudCorpusSearchHit> {
    let mut deduped: HashMap<String, CloudCorpusSearchHit> = HashMap::new();
    for hit in hits {
        let key = if !hit.identity_key.is_empty() {
            hit.identity_key.clone()
        } else {
            hit.id.clone()
        };
        if let Some(existing) = deduped.get(&key) {
            let prefer_existing_private = share_mode == ShareMode::Private
                && include_community_overlay
                && existing.visibility == "private";
            let prefer_next_private = share_mode == ShareMode::Private
                && include_community_overlay
                && hit.visibility == "private";
            if prefer_existing_private && !prefer_next_private {
                continue;
            }
            if prefer_next_private && !prefer_existing_private {
                deduped.insert(key, hit);
                continue;
            }
            if hit.score > existing.score {
                deduped.insert(key, hit);
            }
        } else {
            deduped.insert(key, hit);
        }
    }
    let mut results: Vec<CloudCorpusSearchHit> = deduped.into_values().collect();
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.updated_at.cmp(&a.updated_at))
    });
    results.truncate(limit);
    results
}

/// Drain pending sync queue jobs to the remote corpus.
pub async fn drain_sync_queue(
    store: &AppStore,
    cloud_client: &CloudCorpusClient,
    share_mode: ShareMode,
    sync_enabled: bool,
    auth: Option<&CloudCorpusAuthContext>,
) -> Result<DrainSyncResult> {
    if share_mode == ShareMode::Local || !sync_enabled {
        return Ok(DrainSyncResult {
            sent: 0,
            failed: 0,
            skipped: true,
        });
    }
    if !cloud_client.is_configured() {
        return Ok(DrainSyncResult {
            sent: 0,
            failed: 0,
            skipped: true,
        });
    }

    let jobs = {
        let store = store.clone();
        tokio::task::spawn_blocking(move || store.list_sync_jobs_full(200))
            .await??
    };

    // Sync failed attempts first (independent of verified items)
    eprintln!("[sync] Checking {} total jobs for failures...", jobs.len());
    let failure_jobs: Vec<_> = jobs.iter()
        .filter(|j| j.status == "pending" && j.queue_type == "attempt.failure")
        .cloned()
        .collect();
    for job in &failure_jobs {
        if let Ok(payload) = serde_json::from_str::<serde_json::Value>(&job.payload_json) {
            if let Some(attempts) = payload.get("attempts").and_then(|v| v.as_array()) {
                for attempt in attempts {
                    if let Err(e) = cloud_client.upload_failed_attempt(attempt.clone()).await {
                        eprintln!("[sync] Failed attempt upload error: {e}");
                    }
                }
            }
        }
        let store = store.clone();
        let jid = job.id.clone();
        let _ = tokio::task::spawn_blocking(move || store.mark_sync_job_status(&jid, "sent"))
            .await;
    }
    if !failure_jobs.is_empty() {
        eprintln!("[sync] Uploaded {} failed attempt(s) to cloud", failure_jobs.len());
    }

    let pending_jobs: Vec<_> = jobs
        .into_iter()
        .filter(|j| j.status == "pending" && j.queue_type == "corpus.contribute")
        .collect();

    if pending_jobs.is_empty() {
        return Ok(DrainSyncResult {
            sent: failure_jobs.len(),
            failed: 0,
            skipped: failure_jobs.is_empty(),
        });
    }

    // Extract identity keys from job payloads
    let mut identity_keys = Vec::new();
    for job in &pending_jobs {
        if let Ok(payload) = serde_json::from_str::<serde_json::Value>(&job.payload_json) {
            if let Some(keys) = payload
                .get("itemIdentityKeys")
                .and_then(|v| v.as_array())
            {
                for k in keys {
                    if let Some(s) = k.as_str() {
                        identity_keys.push(s.to_string());
                    }
                }
            }
        }
    }

    let visibility_scope = if share_mode == ShareMode::Private {
        "private"
    } else {
        "community"
    };

    let candidates = {
        let store = store.clone();
        let keys = identity_keys.clone();
        let vis = visibility_scope.to_string();
        tokio::task::spawn_blocking(move || {
            store.list_verified_upload_candidates(256, &keys, &vis)
        })
        .await??
    };

    if candidates.is_empty() {
        // Mark all jobs as sent (nothing to upload)
        for job in &pending_jobs {
            let store = store.clone();
            let jid = job.id.clone();
            let _ = tokio::task::spawn_blocking(move || store.mark_sync_job_status(&jid, "sent"))
                .await;
        }
        return Ok(DrainSyncResult {
            sent: 0,
            failed: 0,
            skipped: false,
        });
    }

    let items: Vec<CloudCorpusUploadItem> = candidates
        .iter()
        .map(|c| CloudCorpusUploadItem {
            identity_key: c.identity_key.clone(),
            label: c.label.clone(),
            statement: c.statement.clone(),
            artifact_content: c.artifact_content.clone(),
            artifact_id: Some(c.artifact_id.clone()),
            verification_run_id: Some(c.verification_run_id.clone()),
            decl_name: c.decl_name.clone(),
            module_name: c.module_name.clone(),
            package_name: c.package_name.clone(),
            package_revision: c.package_revision.clone(),
            decl_kind: Some(c.decl_kind.clone()),
            doc_string: c.doc_string.clone(),
            namespace: c.namespace.clone(),
            imports: c.imports.clone(),
            environment_fingerprint: c.environment_fingerprint.clone(),
            is_theorem_like: c.is_theorem_like,
            is_instance: c.is_instance,
            metadata: c.metadata.clone(),
        })
        .collect();

    let result = match cloud_client
        .upload_verified_batch(visibility_scope, items, auth)
        .await
    {
        Ok(_) => {
            for job in &pending_jobs {
                let store = store.clone();
                let jid = job.id.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    store.mark_sync_job_status(&jid, "sent")
                })
                .await;
            }
            DrainSyncResult {
                sent: candidates.len(),
                failed: 0,
                skipped: false,
            }
        }
        Err(_) => {
            for job in &pending_jobs {
                let store = store.clone();
                let jid = job.id.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    store.mark_sync_job_status(&jid, "failed")
                })
                .await;
            }
            DrainSyncResult {
                sent: 0,
                failed: pending_jobs.len(),
                skipped: false,
            }
        }
    };

    Ok(result)
}
