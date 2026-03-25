//! Terminal setup, `run_shell`, and simple headless commands.
//!
//! `run_shell` is the entry point for the interactive TUI.  It opens the
//! store, kicks off background auth/lean-health tasks, installs a panic hook
//! that restores the terminal, then hands off to `event_loop::run_app`.

use crate::event_loop::run_app;
use crate::helpers::resolve_lean_project_dir;
use anyhow::{bail, Result};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use openproof_core::{AppEvent, AppState};
use openproof_dashboard::{open_browser, start_dashboard_server};
use openproof_lean::detect_lean_health;
use openproof_model::{load_auth_summary, sync_auth_from_codex_cli};
use openproof_protocol::HealthReport;
use openproof_store::{AppStore, StorePaths};
use ratatui::backend::CrosstermBackend;
use std::{io, io::Write as _, path::PathBuf};
use tokio::sync::mpsc;

pub async fn run_shell(launch_cwd: PathBuf) -> Result<()> {
    let store = AppStore::open(StorePaths::detect()?)?;
    let import_summary = store.import_legacy_sessions()?;
    let workspace_root = launch_cwd
        .canonicalize()
        .unwrap_or(launch_cwd.clone())
        .to_string_lossy()
        .to_string();
    let workspace_label = launch_cwd
        .file_name()
        .and_then(|value| value.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| "workspace".to_string());
    let sessions = store
        .list_sessions()?
        .into_iter()
        .filter(|session| session.workspace_root.as_deref() == Some(workspace_root.as_str()))
        .collect::<Vec<_>>();
    let mut state = AppState::new(
        sessions,
        format!(
            "Imported {} legacy sessions ({} skipped, {} failed).",
            import_summary.imported, import_summary.skipped, import_summary.failed
        ),
        Some(workspace_root),
        Some(workspace_label),
    );

    // Apply setup config: set default share mode based on corpus_mode
    if let Some(config) = crate::setup::load_config() {
        if config.corpus_mode == "cloud" {
            // Default new sessions to community mode for cloud corpus
            if let Some(session) = state.current_session_mut() {
                if session.cloud.share_mode == openproof_protocol::ShareMode::Local {
                    session.cloud.share_mode = openproof_protocol::ShareMode::Community;
                    session.cloud.sync_enabled = true;
                }
            }
        }
        // Set corpus URL from config if present (overrides hardcoded default)
        if let Some(url) = &config.corpus_url {
            std::env::set_var("OPENPROOF_CORPUS_URL", url);
            std::env::set_var("OPENPROOF_ENABLE_REMOTE_CORPUS", "1");
        }
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();

    {
        let tx = tx.clone();
        tokio::spawn(async move {
            let auth = tokio::task::spawn_blocking(load_auth_summary)
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or_default();
            let _ = tx.send(AppEvent::AuthLoaded(auth));
        });
    }

    {
        let tx = tx.clone();
        let lean_project_dir = resolve_lean_project_dir();
        tokio::spawn(async move {
            let lean = tokio::task::spawn_blocking(move || detect_lean_health(&lean_project_dir))
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or_default();
            let _ = tx.send(AppEvent::LeanLoaded(lean));
        });
    }

    // Install panic hook to restore terminal on crash.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = crossterm::execute!(
            io::stderr(),
            crossterm::event::DisableBracketedPaste,
            crossterm::cursor::Show,
        );
        let _ = write!(io::stderr(), "\x1b[r");
        original_hook(info);
    }));

    // Build OpenProof.Corpus module from cloud + local corpus data (background).
    // Non-blocking: TUI starts immediately, corpus compiles in parallel.
    {
        let pd = resolve_lean_project_dir();
        let store_for_corpus = store.clone();
        tokio::spawn(async move {
            let mut all_items = Vec::new();

            // Try cloud first
            let cloud = openproof_cloud::CloudCorpusClient::new(Default::default());
            if cloud.is_configured() {
                let mut offset = 0usize;
                loop {
                    match cloud.fetch_all_user_verified(500, offset).await {
                        Ok((items, total)) => {
                            eprintln!("[corpus-module] Cloud: fetched {} items (total: {total})", items.len());
                            let n = items.len();
                            for item in items {
                                all_items.push(openproof_lean::corpus_module::CorpusDeclaration {
                                    label: item.label,
                                    statement: item.statement,
                                    artifact_content: item.artifact_content,
                                });
                            }
                            if n < 500 { break; }
                            offset += 500;
                        }
                        Err(e) => {
                            eprintln!("[corpus-module] Cloud fetch failed: {e}");
                            break;
                        }
                    }
                }
            }

            // Always supplement with local user-verified items (may have items not yet synced)
            let local_result = tokio::task::spawn_blocking(move || {
                store_for_corpus.list_user_verified_with_artifacts()
            }).await;
            if let Ok(Ok(local_items)) = local_result {
                for (label, statement, content) in local_items {
                    if !all_items.iter().any(|i| i.label == label) {
                        all_items.push(openproof_lean::corpus_module::CorpusDeclaration {
                            label, statement, artifact_content: content,
                        });
                    }
                }
            }

            if all_items.is_empty() {
                eprintln!("[corpus-module] No corpus items found (cloud + local)");
                return;
            }

            eprintln!("[corpus-module] Building module with {} declarations", all_items.len());
            let _ = tokio::task::spawn_blocking(move || {
                openproof_lean::corpus_module::build_corpus_module(&pd, &all_items)
            }).await;
        });
    }

    // Spawn Pantograph in background (loads Mathlib ~18s).
    // TUI launches immediately. Tool calls wait for it via the OnceCell.
    let lean_project_dir = resolve_lean_project_dir();
    let session_prover: std::sync::Arc<std::sync::OnceLock<openproof_lean::proof_tree::SharedProver>> =
        std::sync::Arc::new(std::sync::OnceLock::new());
    {
        let slot = session_prover.clone();
        let pd = lean_project_dir.clone();
        std::thread::spawn(move || {
            if let Ok(sp) = openproof_lean::proof_tree::SessionProver::spawn(&pd) {
                let _ = slot.set(std::sync::Arc::new(std::sync::Mutex::new(sp)));
            }
        });
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    write!(stdout, "\x1b[r\x1b[0m\x1b[H\x1b[2J\x1b[3J\x1b[H")?;
    crossterm::execute!(stdout, crossterm::event::EnableBracketedPaste)?;
    stdout.flush()?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = openproof_tui::custom_terminal::CustomTerminal::with_options(backend)?;
    let size = terminal.size()?;
    terminal.set_viewport_area(ratatui::layout::Rect::new(0, 0, size.width, size.height));
    let app_result = run_app(&mut terminal, store, &mut state, tx, &mut rx, session_prover.clone()).await;
    let _ = crossterm::execute!(terminal.backend_mut(), crossterm::event::DisableBracketedPaste);
    disable_raw_mode()?;
    terminal.show_cursor()?;
    terminal.clear()?;
    let vp = terminal.viewport_area;
    let _ = crossterm::execute!(
        terminal.backend_mut(),
        crossterm::cursor::MoveTo(0, vp.bottom()),
    );
    let _ = std::panic::take_hook();
    app_result
}

pub async fn build_health_report(launch_cwd: PathBuf) -> Result<HealthReport> {
    let store = AppStore::open(StorePaths::detect()?)?;
    let auth = load_auth_summary().unwrap_or_default();
    let _ = launch_cwd;
    let lean = detect_lean_health(&resolve_lean_project_dir()).unwrap_or_default();
    let latest_session = store.latest_session()?;
    Ok(HealthReport {
        ok: lean.ok,
        local_db_path: store.db_path().display().to_string(),
        session_count: store.session_count()?,
        latest_session_id: latest_session.map(|session| session.id),
        auth,
        lean,
    })
}

pub async fn run_health(launch_cwd: PathBuf) -> Result<()> {
    let report = build_health_report(launch_cwd).await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

pub async fn run_login() -> Result<()> {
    match sync_auth_from_codex_cli()? {
        Some(summary) => {
            println!("{}", serde_json::to_string_pretty(&summary)?);
            Ok(())
        }
        None => {
            bail!("No reusable Codex CLI ChatGPT login was found.");
        }
    }
}

pub async fn run_ask(prompt: String) -> Result<()> {
    use openproof_model::{run_codex_turn, CodexTurnRequest, TurnMessage};

    let session_id = format!("ask_{}", chrono::Utc::now().timestamp_millis());
    let response = run_codex_turn(CodexTurnRequest {
        session_id: &session_id,
        messages: &[
            TurnMessage::chat("system", "You are openproof, a concise formal math assistant."),
            TurnMessage::chat("user", prompt),
        ],
        model: "gpt-5.4",
        reasoning_effort: "high",
        include_tools: false,
    })
    .await?;
    println!("{response}");
    Ok(())
}

pub async fn run_dashboard(
    launch_cwd: PathBuf,
    should_open: bool,
    port: Option<u16>,
) -> Result<()> {
    let store = AppStore::open(StorePaths::detect()?)?;
    let _ = launch_cwd;
    let lean_project_dir = resolve_lean_project_dir();
    let server = start_dashboard_server(store, lean_project_dir, port).await?;
    let url = format!("http://127.0.0.1:{}", server.port);
    println!("openproof dashboard listening on {url}");
    if should_open {
        open_browser(&url);
    }
    tokio::signal::ctrl_c().await?;
    server.close().await?;
    Ok(())
}

pub async fn run_recluster_corpus() -> Result<()> {
    let store = AppStore::open(StorePaths::detect()?)?;
    let summary = store.rebuild_verified_corpus_clusters()?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

pub async fn run_ingest_corpus() -> Result<()> {
    let store = AppStore::open(StorePaths::detect()?)?;
    let lean_root = crate::helpers::resolve_lean_project_dir();
    eprintln!("Ingesting library seeds from {}...", lean_root.display());
    let results = tokio::task::spawn_blocking(move || {
        store.ingest_default_library_seeds(&lean_root)
    }).await??;
    if results.is_empty() {
        eprintln!("No library seed packages found.");
    } else {
        for (pkg, count) in &results {
            eprintln!("{pkg}: {count} declarations");
        }
    }
    let store2 = AppStore::open(StorePaths::detect()?)?;
    let summary = store2.get_corpus_summary()?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}
