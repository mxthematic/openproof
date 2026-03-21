//! Sub-command handlers for `/share`, `/corpus`, `/sync`.

use crate::helpers::{
    describe_remote_corpus, emit_local_notice, parse_share_mode, persist_write, share_mode_label,
};
use openproof_core::{AppEvent, AppState};
use openproof_store::AppStore;
use std::{env, path::PathBuf, result::Result};
use tokio::sync::mpsc;

pub fn cmd_share(
    tx: mpsc::UnboundedSender<AppEvent>,
    state: &mut AppState,
    store: AppStore,
    arg_text: &str,
) {
    use openproof_protocol::ShareMode;

    if arg_text.is_empty() {
        let content = state
            .current_session()
            .map(|session| {
                [
                    format!("Share mode: {}", share_mode_label(session.cloud.share_mode)),
                    format!(
                        "Sync enabled: {}",
                        if session.cloud.sync_enabled { "yes" } else { "no" }
                    ),
                    format!(
                        "Private overlay community: {}",
                        if session.cloud.private_overlay_community {
                            "on"
                        } else {
                            "off"
                        }
                    ),
                    format!(
                        "Last sync: {}",
                        session
                            .cloud
                            .last_sync_at
                            .clone()
                            .unwrap_or_else(|| "never".to_string())
                    ),
                    format!("Remote corpus: {}", describe_remote_corpus()),
                ]
                .join("\n")
            })
            .unwrap_or_else(|| "No active session.".to_string());
        emit_local_notice(tx, state, store, "Share", content);
        return;
    }
    if let Some(rest) = arg_text.strip_prefix("overlay") {
        let value = rest.trim();
        let enable = match value {
            "on" => true,
            "off" => false,
            _ => {
                emit_local_notice(
                    tx,
                    state,
                    store,
                    "Share Usage",
                    "Usage: /share overlay [on|off]".to_string(),
                );
                return;
            }
        };
        let current_share_mode = state
            .current_session()
            .map(|session| session.cloud.share_mode)
            .unwrap_or(ShareMode::Local);
        if current_share_mode != ShareMode::Private {
            emit_local_notice(
                tx,
                state,
                store,
                "Share Error",
                "Private overlay only applies when share mode is private.".to_string(),
            );
            return;
        }
        match state.set_private_overlay_community(enable) {
            Ok(write) => {
                persist_write(tx.clone(), store.clone(), write);
                emit_local_notice(
                    tx,
                    state,
                    store,
                    "Share",
                    if enable {
                        "Private corpus will also search community results.".to_string()
                    } else {
                        "Private corpus will stay isolated from community results.".to_string()
                    },
                );
            }
            Err(error) => emit_local_notice(tx, state, store, "Share Error", error),
        }
        return;
    }
    match parse_share_mode(arg_text) {
        Some(share_mode) => match state.set_share_mode(share_mode) {
            Ok(write) => {
                persist_write(tx.clone(), store.clone(), write);
                emit_local_notice(
                    tx,
                    state,
                    store,
                    "Share",
                    format!("Share mode set to {}.", share_mode_label(share_mode)),
                );
            }
            Err(error) => emit_local_notice(tx, state, store, "Share Error", error),
        },
        None => emit_local_notice(
            tx,
            state,
            store,
            "Share Usage",
            "Usage: /share [local|community|private] or /share overlay [on|off]".to_string(),
        ),
    }
}

pub fn cmd_corpus(
    tx: mpsc::UnboundedSender<AppEvent>,
    state: &mut AppState,
    store: AppStore,
    arg_text: &str,
) {
    let mut parts = arg_text.splitn(2, ' ');
    let subcommand = parts.next().unwrap_or("status").trim();
    let rest = parts.next().unwrap_or("").trim();
    match subcommand {
        "" | "status" => match store.get_corpus_summary() {
            Ok(summary) => emit_local_notice(
                tx,
                state,
                store,
                "Corpus",
                [
                    format!("Verified entries: {}", summary.verified_entry_count),
                    format!("User verified: {}", summary.user_verified_count),
                    format!("Library seed: {}", summary.library_seed_count),
                    format!("Clusters: {}", summary.cluster_count),
                    format!("Duplicate members: {}", summary.duplicate_member_count),
                    format!("Attempt memory: {}", summary.attempt_log_count),
                    format!(
                        "Latest update: {}",
                        summary
                            .latest_updated_at
                            .unwrap_or_else(|| "never".to_string())
                    ),
                ]
                .join("\n"),
            ),
            Err(error) => {
                emit_local_notice(tx, state, store, "Corpus Error", error.to_string())
            }
        },
        "search" => {
            if rest.is_empty() {
                emit_local_notice(
                    tx,
                    state,
                    store,
                    "Corpus Usage",
                    "Usage: /corpus search <query>".to_string(),
                );
                return;
            }
            match store.search_verified_corpus(rest, 8) {
                Ok(hits) if hits.is_empty() => emit_local_notice(
                    tx,
                    state,
                    store,
                    "Corpus Search",
                    "No verified corpus hits matched that query.".to_string(),
                ),
                Ok(hits) => emit_local_notice(
                    tx,
                    state,
                    store,
                    "Corpus Search",
                    hits.into_iter()
                        .map(|(label, statement, visibility)| {
                            format!("{label} [{visibility}] :: {statement}")
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                Err(error) => {
                    emit_local_notice(tx, state, store, "Corpus Error", error.to_string())
                }
            }
        }
        "ingest" => {
            emit_local_notice(
                tx.clone(),
                state,
                store.clone(),
                "Corpus Ingest",
                "Seeding the native verified corpus from local Lean libraries in the background."
                    .to_string(),
            );
            let tx_ingest = tx.clone();
            let store_ingest = store.clone();
            let lean_root = env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join("lean");
            tokio::spawn(async move {
                let outcome = tokio::task::spawn_blocking(move || {
                    store_ingest.ingest_default_library_seeds(&lean_root)
                })
                .await
                .ok()
                .and_then(Result::ok);
                match outcome {
                    Some(results) => {
                        let summary = if results.is_empty() {
                            "No local library seed packages were found.".to_string()
                        } else {
                            results
                                .into_iter()
                                .map(|(package, count)| {
                                    format!("{package}: {count} declarations")
                                })
                                .collect::<Vec<_>>()
                                .join("\n")
                        };
                        let _ = tx_ingest.send(AppEvent::AppendNotice {
                            title: "Corpus Ingest Complete".to_string(),
                            content: summary,
                        });
                    }
                    None => {
                        let _ = tx_ingest.send(AppEvent::AppendNotice {
                            title: "Corpus Ingest Error".to_string(),
                            content: "Library-seed ingestion failed.".to_string(),
                        });
                    }
                }
            });
        }
        "recluster" => match store.rebuild_verified_corpus_clusters() {
            Ok(summary) => emit_local_notice(
                tx,
                state,
                store,
                "Corpus Recluster",
                [
                    "Rebuilt verified corpus clusters.".to_string(),
                    format!("Clusters: {}", summary.cluster_count),
                    format!("Duplicate members: {}", summary.duplicate_member_count),
                    format!("Verified entries: {}", summary.verified_entry_count),
                ]
                .join("\n"),
            ),
            Err(error) => emit_local_notice(
                tx,
                state,
                store,
                "Corpus Recluster Error",
                error.to_string(),
            ),
        },
        _ => emit_local_notice(
            tx,
            state,
            store,
            "Corpus Usage",
            "Usage: /corpus status|search <query>|ingest|recluster".to_string(),
        ),
    }
}

pub fn cmd_sync(
    tx: mpsc::UnboundedSender<AppEvent>,
    state: &mut AppState,
    store: AppStore,
    arg_text: &str,
) {
    let subcommand = if arg_text.is_empty() { "status" } else { arg_text };
    match subcommand {
        "status" => match store.get_sync_summary() {
            Ok(summary) => {
                let content = state
                    .current_session()
                    .map(|session| {
                        [
                            format!("Share mode: {}", share_mode_label(session.cloud.share_mode)),
                            format!(
                                "Sync enabled: {}",
                                if session.cloud.sync_enabled { "yes" } else { "no" }
                            ),
                            format!("Pending jobs: {}", summary.pending_count),
                            format!("Failed jobs: {}", summary.failed_count),
                            format!("Sent jobs: {}", summary.sent_count),
                            format!(
                                "Last sync: {}",
                                session
                                    .cloud
                                    .last_sync_at
                                    .clone()
                                    .unwrap_or_else(|| "never".to_string())
                            ),
                            format!("Remote corpus: {}", describe_remote_corpus()),
                        ]
                        .join("\n")
                    })
                    .unwrap_or_else(|| "No active session.".to_string());
                emit_local_notice(tx, state, store, "Sync", content);
            }
            Err(error) => emit_local_notice(tx, state, store, "Sync Error", error.to_string()),
        },
        "enable" => match state.set_sync_enabled(true) {
            Ok(write) => {
                persist_write(tx.clone(), store.clone(), write);
                emit_local_notice(
                    tx,
                    state,
                    store,
                    "Sync",
                    "Enabled sync for the current session.".to_string(),
                );
            }
            Err(error) => emit_local_notice(tx, state, store, "Sync Error", error),
        },
        "disable" => match state.set_sync_enabled(false) {
            Ok(write) => {
                persist_write(tx.clone(), store.clone(), write);
                emit_local_notice(
                    tx,
                    state,
                    store,
                    "Sync",
                    "Disabled sync for the current session.".to_string(),
                );
            }
            Err(error) => emit_local_notice(tx, state, store, "Sync Error", error),
        },
        "drain" => {
            start_sync_drain(tx, state, store);
        }
        _ => emit_local_notice(
            tx,
            state,
            store,
            "Sync Usage",
            "Usage: /sync status|enable|disable|drain".to_string(),
        ),
    }
}

pub fn start_sync_drain(
    tx: mpsc::UnboundedSender<AppEvent>,
    state: &mut AppState,
    store: AppStore,
) {
    let Some(session) = state.current_session().cloned() else {
        emit_local_notice(tx, state, store, "Sync Error", "No active session.".to_string());
        return;
    };
    if !session.cloud.sync_enabled {
        emit_local_notice(
            tx,
            state,
            store,
            "Sync Error",
            "Sync is disabled for the current session.".to_string(),
        );
        return;
    }
    let cloud_client = openproof_cloud::CloudCorpusClient::new(Default::default());
    if !cloud_client.is_configured() {
        emit_local_notice(
            tx,
            state,
            store,
            "Sync Error",
            "Remote corpus is not configured. Set OPENPROOF_ENABLE_REMOTE_CORPUS=1 and OPENPROOF_CORPUS_URL."
                .to_string(),
        );
        return;
    }
    let desc = cloud_client.describe();
    emit_local_notice(
        tx.clone(),
        state,
        store.clone(),
        "Sync",
        format!("Draining pending sync jobs to {desc} in the background."),
    );
    let share_mode = session.cloud.share_mode;
    let sync_enabled = session.cloud.sync_enabled;
    tokio::spawn(async move {
        let corpus = openproof_corpus::CorpusManager::new(store, cloud_client, PathBuf::from("."));
        match corpus.drain_sync_queue(share_mode, sync_enabled, None).await {
            Ok(result) => {
                if result.sent > 0 {
                    let _ = tx.send(AppEvent::SyncCompleted);
                }
                let _ = tx.send(AppEvent::AppendNotice {
                    title: "Sync".to_string(),
                    content: format!(
                        "Sync drain finished. Sent {} job(s); failed {}.",
                        result.sent, result.failed
                    ),
                });
            }
            Err(e) => {
                let _ = tx.send(AppEvent::AppendNotice {
                    title: "Sync Error".to_string(),
                    content: format!("Sync drain failed: {e}"),
                });
            }
        }
    });
}
