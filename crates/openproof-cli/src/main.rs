use anyhow::{bail, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode},
};
use directories::BaseDirs;
use openproof_core::{AppEvent, AppState, AutonomousRunPatch, FocusPane, SubmittedInput};
use openproof_dashboard::{open_browser, start_dashboard_server};
use openproof_lean::{detect_lean_health, verify_active_node};
use openproof_model::{
    load_auth_summary, run_codex_turn, sync_auth_from_codex_cli, CodexTurnRequest, TurnMessage,
};
use openproof_protocol::{
    AgentRole, AgentStatus, BranchQueueState, HealthReport, MessageRole, ProofNodeKind,
    SessionSnapshot, ShareMode, TranscriptEntry,
};
use openproof_store::{AppStore, StorePaths};
use ratatui::backend::CrosstermBackend;
use std::{
    env, fs, io,
    io::Write as _,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::sync::mpsc;

enum Command {
    Shell,
    Health,
    Login,
    Ask { prompt: String },
    Run { problem: String, label: Option<String> },
    Dashboard { open: bool, port: Option<u16> },
    ReclusterCorpus,
    Help,
}

struct CliOptions {
    command: Command,
    launch_cwd: PathBuf,
}

#[derive(Debug, Clone, Default)]
struct PromptContextFiles {
    instructions: String,
    global_memory_path: PathBuf,
    workspace_memory_path: PathBuf,
    memory: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let options = parse_args(env::args().skip(1).collect::<Vec<_>>())?;
    match options.command {
        Command::Help => {
            print_help();
            Ok(())
        }
        Command::Health => run_health(options.launch_cwd).await,
        Command::Login => run_login().await,
        Command::Ask { prompt } => run_ask(prompt).await,
        Command::Run { problem, label } => run_autonomous(options.launch_cwd, problem, label).await,
        Command::Dashboard { open, port } => run_dashboard(options.launch_cwd, open, port).await,
        Command::ReclusterCorpus => run_recluster_corpus().await,
        Command::Shell => run_shell(options.launch_cwd).await,
    }
}

fn parse_args(args: Vec<String>) -> Result<CliOptions> {
    let launch_cwd = env::var("OPENPROOF_LAUNCH_CWD")
        .map(PathBuf::from)
        .unwrap_or_else(|_| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    if args.is_empty() {
        return Ok(CliOptions {
            command: Command::Shell,
            launch_cwd,
        });
    }

    if args
        .iter()
        .any(|arg| arg == "--help" || arg == "-h" || arg == "help")
    {
        return Ok(CliOptions {
            command: Command::Help,
            launch_cwd,
        });
    }

    if args.iter().any(|arg| arg == "--health")
        || args.first().map(String::as_str) == Some("health")
    {
        return Ok(CliOptions {
            command: Command::Health,
            launch_cwd,
        });
    }

    if args.iter().any(|arg| arg == "--login") || args.first().map(String::as_str) == Some("login")
    {
        return Ok(CliOptions {
            command: Command::Login,
            launch_cwd,
        });
    }

    if args.iter().any(|arg| arg == "--recluster-corpus")
        || args.first().map(String::as_str) == Some("recluster-corpus")
    {
        return Ok(CliOptions {
            command: Command::ReclusterCorpus,
            launch_cwd,
        });
    }

    if args.first().map(String::as_str) == Some("dashboard") {
        let mut open = false;
        let mut port = None;
        let mut index = 1;
        while index < args.len() {
            match args[index].as_str() {
                "--open" => {
                    open = true;
                }
                "--port" => {
                    let Some(value) = args.get(index + 1) else {
                        bail!("dashboard --port requires a value");
                    };
                    port = Some(value.parse::<u16>()?);
                    index += 1;
                }
                unexpected => bail!("unknown dashboard argument: {unexpected}"),
            }
            index += 1;
        }
        return Ok(CliOptions {
            command: Command::Dashboard { open, port },
            launch_cwd,
        });
    }

    if args.first().map(String::as_str) == Some("ask") {
        let prompt = args.iter().skip(1).cloned().collect::<Vec<_>>().join(" ");
        if prompt.trim().is_empty() {
            bail!("openproof ask requires a prompt");
        }
        return Ok(CliOptions {
            command: Command::Ask { prompt },
            launch_cwd,
        });
    }

    if args.first().map(String::as_str) == Some("run") {
        let mut problem = String::new();
        let mut label = None;
        let mut index = 1;
        while index < args.len() {
            match args[index].as_str() {
                "--label" => {
                    index += 1;
                    label = args.get(index).cloned();
                }
                "--problem" => {
                    index += 1;
                    if let Some(p) = args.get(index) {
                        problem = p.clone();
                    }
                }
                other if problem.is_empty() => {
                    problem = other.to_string();
                }
                _ => {}
            }
            index += 1;
        }
        if problem.trim().is_empty() {
            bail!("openproof run requires a problem statement. Usage: openproof run \"<problem>\" [--label <name>]");
        }
        return Ok(CliOptions {
            command: Command::Run { problem, label },
            launch_cwd,
        });
    }

    Ok(CliOptions {
        command: Command::Shell,
        launch_cwd,
    })
}

fn is_lean_project_dir(dir: &Path) -> bool {
    dir.join("lakefile.lean").exists() || dir.join("lakefile.toml").exists()
}

fn resolve_lean_project_dir() -> PathBuf {
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if is_lean_project_dir(&cwd) {
        return cwd;
    }
    let lean_sub = cwd.join("lean");
    if is_lean_project_dir(&lean_sub) {
        return lean_sub;
    }
    if let Ok(launch) = env::var("OPENPROOF_LAUNCH_CWD") {
        let launch_lean = PathBuf::from(&launch).join("lean");
        if is_lean_project_dir(&launch_lean) {
            return launch_lean;
        }
    }
    lean_sub
}

fn print_help() {
    println!(
        "\
openproof

Usage:
  openproof
  openproof health
  openproof login
  openproof ask <prompt>
  openproof run <problem> [--label <name>]
  openproof dashboard [--open] [--port <port>]
  openproof recluster-corpus

Legacy flags:
  --health    same as `openproof health`
  --login     same as `openproof login`
  --recluster-corpus same as `openproof recluster-corpus`
  --help      show this help"
    );
}

async fn run_health(launch_cwd: PathBuf) -> Result<()> {
    let report = build_health_report(launch_cwd).await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

async fn run_login() -> Result<()> {
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

async fn run_ask(prompt: String) -> Result<()> {
    let session_id = format!("ask_{}", chrono::Utc::now().timestamp_millis());
    let response = run_codex_turn(CodexTurnRequest {
        session_id: &session_id,
        messages: &[
            TurnMessage {
                role: "system".to_string(),
                content: "You are openproof, a concise formal math assistant.".to_string(),
            },
            TurnMessage {
                role: "user".to_string(),
                content: prompt,
            },
        ],
        model: "gpt-5.4",
        reasoning_effort: "high",
    })
    .await?;
    println!("{response}");
    Ok(())
}

async fn run_dashboard(launch_cwd: PathBuf, should_open: bool, port: Option<u16>) -> Result<()> {
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

/// Headless autonomous mode: set up a session, run the autonomous proof loop,
/// and print all state changes to stderr. No TUI required.
async fn run_autonomous(launch_cwd: PathBuf, problem: String, label: Option<String>) -> Result<()> {
    let store = AppStore::open(StorePaths::detect()?)?;
    let _ = store.import_legacy_sessions();
    let workspace_label = launch_cwd
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "workspace".to_string());

    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();

    let sessions = store.list_sessions()?;
    let wr = Some(launch_cwd.to_string_lossy().to_string());
    let wl = Some(workspace_label.clone());
    let mut state = AppState::new(sessions, String::new(), wr.clone(), wl.clone());

    let title = label.unwrap_or_else(|| {
        let preview: String = problem.chars().take(60).collect();
        format!("auto: {preview}")
    });
    let write = state.create_session(Some(&title));
    persist_write(tx.clone(), store.clone(), write);

    // Load auth
    eprintln!("[run] Loading auth...");
    match sync_auth_from_codex_cli() {
        Ok(Some(summary)) => {
            let _ = state.apply(AppEvent::AuthLoaded(summary));
        }
        _ => {
            if let Ok(summary) = load_auth_summary() {
                let _ = state.apply(AppEvent::AuthLoaded(summary));
            }
        }
    }
    if !state.auth.logged_in {
        bail!("Not authenticated. Run `openproof login` first.");
    }
    eprintln!("[run] Auth: {} ({})",
        state.auth.email.as_deref().unwrap_or("unknown"),
        state.auth.plan.as_deref().unwrap_or("?"));

    // Load lean health
    let lean_dir = resolve_lean_project_dir();
    let lean_dir_clone = lean_dir.clone();
    if let Ok(health) = tokio::task::spawn_blocking(move || detect_lean_health(&lean_dir_clone)).await? {
        let _ = state.apply(AppEvent::LeanLoaded(health));
    }
    eprintln!("[run] Lean: ok={}, version={}",
        state.lean.ok,
        state.lean.lean_version.as_deref().unwrap_or("?"));

    // Submit problem as user message
    eprintln!("[run] Problem: {problem}");
    let submitted = state.submit_text(problem.clone());
    if let Some(_input) = submitted {
        if let Some(session) = state.current_session().cloned() {
            let s = store.clone();
            let _ = tokio::task::spawn_blocking(move || s.save_session(&session)).await;
        }

        // Initial model turn
        let session = state.current_session().cloned().unwrap();
        let messages = build_turn_messages_with_retrieval(&store, Some(&session)).await;
        eprintln!("[run] Running initial model turn...");
        let _ = state.apply(AppEvent::TurnStarted);
        match run_codex_turn(CodexTurnRequest {
            session_id: &session.id,
            messages: &messages,
            model: "gpt-5.4",
            reasoning_effort: "high",
        }).await {
            Ok(text) => {
                eprintln!("[run] Response ({} chars, {} lines)", text.len(), text.lines().count());
                for line in text.lines().take(15) {
                    eprintln!("  | {line}");
                }
                if text.lines().count() > 15 {
                    eprintln!("  | ... ({} more lines)", text.lines().count() - 15);
                }
                let _ = state.apply(AppEvent::AppendAssistant(text));
                let _ = state.apply(AppEvent::TurnFinished);
            }
            Err(e) => {
                eprintln!("[run] Model error: {e}");
                let _ = state.apply(AppEvent::TurnFinished);
            }
        }
        if let Some(session) = state.current_session().cloned() {
            let s = store.clone();
            let _ = tokio::task::spawn_blocking(move || s.save_session(&session)).await;
        }
    }

    // Report extracted state
    let session = state.current_session().cloned().unwrap();
    eprintln!("[run] Phase: {}", session.proof.phase);
    eprintln!("[run] Formal target: {:?}", session.proof.formal_target);
    eprintln!("[run] Accepted target: {:?}", session.proof.accepted_target);
    eprintln!("[run] Nodes: {}", session.proof.nodes.len());
    for node in &session.proof.nodes {
        eprintln!("[run]   {} [{:?}]: {}", node.label, node.status, node.statement);
    }

    if session.proof.formal_target.is_none() && session.proof.accepted_target.is_none() && session.proof.nodes.is_empty() {
        eprintln!("[run] No target extracted. Adding theorem node from problem.");
        let _ = state.add_proof_node(ProofNodeKind::Theorem, &title, &problem);
    }

    // If we have a node but no accepted target, auto-accept so autonomous can proceed
    let session = state.current_session().cloned().unwrap();
    if session.proof.accepted_target.is_none() {
        let target = session.proof.formal_target.clone()
            .or_else(|| session.proof.nodes.first().map(|n| n.statement.clone()))
            .unwrap_or_else(|| problem.clone());
        eprintln!("[run] Auto-accepting target: {}", target.chars().take(100).collect::<String>());
        if let Some(s) = state.current_session_mut() {
            s.proof.accepted_target = Some(target);
            s.proof.phase = "proving".to_string();
        }
        if let Some(session) = state.current_session().cloned() {
            let s = store.clone();
            let _ = tokio::task::spawn_blocking(move || s.save_session(&session)).await;
        }
    }

    // Start autonomous
    eprintln!("\n[run] === Starting autonomous loop ===\n");
    if let Ok(write) = state.set_autonomous_run_state(AutonomousRunPatch {
        is_autonomous_running: Some(true),
        autonomous_iteration_count: Some(0),
        autonomous_started_at: Some(Some(chrono::Utc::now().to_rfc3339())),
        autonomous_pause_reason: Some(None),
        autonomous_stop_reason: Some(None),
        ..AutonomousRunPatch::default()
    }) {
        persist_write(tx.clone(), store.clone(), write);
    }

    let max_iterations = 12;
    for iteration in 1..=max_iterations {
        let session = state.current_session().cloned().unwrap();
        if !session.proof.is_autonomous_running {
            eprintln!("[run] Autonomous stopped: {:?}", session.proof.autonomous_pause_reason);
            break;
        }
        if let Some(reason) = autonomous_stop_reason(&session) {
            eprintln!("[run] Stop: {reason}");
            break;
        }

        eprintln!("\n[run] --- Iteration {iteration}/{max_iterations} ---");
        eprintln!("[run] Phase={}, Branches={}, Nodes={}",
            session.proof.phase, session.proof.branches.len(), session.proof.nodes.len());

        match run_autonomous_step(tx.clone(), store.clone(), &mut state) {
            Ok(summary) => {
                for line in summary.lines() {
                    eprintln!("[run] {line}");
                }
            }
            Err(reason) => {
                eprintln!("[run] Step error: {reason}");
                break;
            }
        }

        // Drain events until all branches settle
        let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
        loop {
            match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
                Ok(Some(event)) => {
                    // Track branches that just finished for verification
                    let mut finished_branch_id: Option<String> = None;
                    match &event {
                        AppEvent::AppendBranchAssistant { branch_id, content } => {
                            let lean_count = content.matches("```lean").count();
                            eprintln!("[run] Branch {branch_id}: {len} chars, {lean_count} lean block(s)",
                                len = content.len());
                        }
                        AppEvent::FinishBranch { branch_id, status, summary, .. } => {
                            eprintln!("[run] Branch {branch_id} finished: {status:?} -- {summary}");
                            finished_branch_id = Some(branch_id.clone());
                        }
                        AppEvent::LeanVerifyStarted => {
                            eprintln!("[run] Lean verification started...");
                        }
                        AppEvent::LeanVerifyFinished(r) => {
                            eprintln!("[run] Verify: ok={}, code={:?}", r.ok, r.code);
                            if !r.ok {
                                for l in r.stderr.lines().take(3) { eprintln!("[run]   {l}"); }
                            }
                        }
                        AppEvent::BranchVerifyFinished { branch_id, result, promote, .. } => {
                            if result.ok {
                                eprintln!("[run] *** BRANCH {branch_id} VERIFIED (promote={promote}) ***");
                            } else {
                                eprintln!("[run] Branch {branch_id} verify failed");
                                for l in result.stderr.lines().take(3) { eprintln!("[run]   {l}"); }
                            }
                        }
                        AppEvent::AppendNotice { title, content } => {
                            eprintln!("[run] {title}: {}", &content[..content.len().min(200)]);
                        }
                        AppEvent::PersistSucceeded(_) | AppEvent::PersistFailed(_) => {}
                        _ => {}
                    }
                    if let Some(write) = state.apply(event) {
                        persist_write(tx.clone(), store.clone(), write);
                    }

                    // After a branch finishes, check if it has lean code and verify
                    if let Some(bid) = finished_branch_id {
                        if let Some(session_snapshot) = state.current_session().cloned() {
                            let branch_info = session_snapshot.proof.branches.iter()
                                .find(|b| b.id == bid)
                                .map(|b| (b.lean_snippet.trim().is_empty(), b.hidden));
                            if let Some((snippet_empty, hidden)) = branch_info {
                                if !snippet_empty {
                                    eprintln!("[run] Branch {} has lean snippet, starting verification...", bid);
                                    start_branch_verification(
                                        tx.clone(),
                                        store.clone(),
                                        session_snapshot,
                                        bid.clone(),
                                        !hidden,
                                    );
                                } else {
                                    eprintln!("[run] Branch {} finished with no lean candidate.", bid);
                                }
                            }
                        }
                    }

                    // Check if settled
                    let s = state.current_session().cloned().unwrap();
                    let all_done = s.proof.branches.iter().all(|b|
                        !matches!(b.status, AgentStatus::Running));
                    if all_done && !state.turn_in_flight && !state.verification_in_flight {
                        break;
                    }
                }
                Ok(None) => break,
                Err(_) => {
                    let s = state.current_session().cloned().unwrap();
                    let running = s.proof.branches.iter().filter(|b| b.status == AgentStatus::Running).count();
                    if running == 0 && !state.turn_in_flight && !state.verification_in_flight {
                        break;
                    }
                    if tokio::time::Instant::now() > deadline {
                        eprintln!("[run] Timeout waiting for tasks.");
                        break;
                    }
                }
            }
        }

        // Persist
        if let Some(session) = state.current_session().cloned() {
            let s = store.clone();
            let _ = tokio::task::spawn_blocking(move || s.save_session(&session)).await;
        }

        // Check for verified nodes
        let session = state.current_session().cloned().unwrap();
        let verified: Vec<_> = session.proof.nodes.iter()
            .filter(|n| n.status == openproof_protocol::ProofNodeStatus::Verified)
            .collect();
        if !verified.is_empty() {
            eprintln!("\n[run] *** {} node(s) VERIFIED ***", verified.len());
            for node in &verified {
                eprintln!("[run] {}: {}", node.label, node.statement);
                eprintln!("{}", node.content);
            }
            break;
        }
    }

    // Final summary
    let session = state.current_session().cloned().unwrap();
    eprintln!("\n[run] === Summary ===");
    eprintln!("[run] Session: {} ({})", session.title, session.id);
    eprintln!("[run] Phase: {}", session.proof.phase);
    eprintln!("[run] Iterations: {}", session.proof.autonomous_iteration_count);
    eprintln!("[run] Nodes: {}", session.proof.nodes.len());
    for n in &session.proof.nodes {
        eprintln!("[run]   {} [{:?}]", n.label, n.status);
    }
    eprintln!("[run] Branches: {}", session.proof.branches.len());
    for b in &session.proof.branches {
        eprintln!("[run]   {} [{:?}] score={:.1} attempts={}", b.title, b.status, b.score, b.attempt_count);
    }
    let corpus = store.get_corpus_summary()?;
    eprintln!("[run] Corpus: verified={}, user_verified={}, attempts={}",
        corpus.verified_entry_count, corpus.user_verified_count, corpus.attempt_log_count);

    // Persist final
    let s = store.clone();
    let _ = tokio::task::spawn_blocking(move || s.save_session(&session)).await;
    Ok(())
}

async fn run_recluster_corpus() -> Result<()> {
    let store = AppStore::open(StorePaths::detect()?)?;
    let summary = store.rebuild_verified_corpus_clusters()?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

async fn run_shell(launch_cwd: PathBuf) -> Result<()> {
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
        let _ = crossterm::execute!(io::stderr(), crossterm::cursor::Show);
        // Reset scroll region in case we crash mid-insert.
        let _ = write!(io::stderr(), "\x1b[r");
        original_hook(info);
    }));

    enable_raw_mode()?;
    // Clear screen and position cursor at top.
    let mut stdout = io::stdout();
    write!(stdout, "\x1b[r\x1b[0m\x1b[H\x1b[2J\x1b[3J\x1b[H")?;
    stdout.flush()?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal =
        openproof_tui::custom_terminal::CustomTerminal::with_options(backend)?;
    // Set viewport to fill the terminal.
    let size = terminal.size()?;
    terminal.set_viewport_area(ratatui::layout::Rect::new(
        0, 0, size.width, size.height,
    ));
    let app_result = run_app(&mut terminal, store, &mut state, tx, &mut rx).await;
    disable_raw_mode()?;
    terminal.show_cursor()?;
    terminal.clear()?;
    // Move cursor below viewport for clean shell prompt.
    let vp = terminal.viewport_area;
    let _ = crossterm::execute!(
        terminal.backend_mut(),
        crossterm::cursor::MoveTo(0, vp.bottom()),
    );
    let _ = std::panic::take_hook();
    app_result
}

async fn build_health_report(launch_cwd: PathBuf) -> Result<HealthReport> {
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

async fn run_app(
    terminal: &mut openproof_tui::custom_terminal::CustomTerminal<CrosstermBackend<io::Stdout>>,
    store: AppStore,
    state: &mut AppState,
    tx: mpsc::UnboundedSender<AppEvent>,
    rx: &mut mpsc::UnboundedReceiver<AppEvent>,
) -> Result<()> {
    loop {
        while let Ok(event) = rx.try_recv() {
            if matches!(event, AppEvent::AutonomousTick) {
                schedule_autonomous_tick(tx.clone(), store.clone(), state);
                continue;
            }
            let verification_result = match &event {
                AppEvent::LeanVerifyFinished(result) => Some(result.clone()),
                _ => None,
            };
            let branch_verification = match &event {
                AppEvent::BranchVerifyFinished {
                    branch_id,
                    focus_node_id,
                    promote,
                    result,
                } => Some((
                    branch_id.clone(),
                    focus_node_id.clone(),
                    *promote,
                    result.clone(),
                )),
                _ => None,
            };
            let finished_branch_id = match &event {
                AppEvent::FinishBranch { branch_id, .. } => Some(branch_id.clone()),
                _ => None,
            };
            if let Some(write) = state.apply(event.clone()) {
                let verification_session = verification_result
                    .as_ref()
                    .map(|_| write.session.clone());
                persist_write(tx.clone(), store.clone(), write);
                if let (Some(result), Some(session)) = (verification_result, verification_session) {
                    persist_verification_result(tx.clone(), store.clone(), session, result);
                }
            }
            if let Some((branch_id, _focus_node_id, _promote, _result)) = branch_verification {
                if state
                    .current_session()
                    .map(|session| session.proof.is_autonomous_running)
                    .unwrap_or(false)
                {
                    let _ = tx.send(AppEvent::AutonomousTick);
                }
                if let Some(branch) = state
                    .current_session()
                    .and_then(|session| session.proof.branches.iter().find(|branch| branch.id == branch_id))
                {
                    if branch.hidden
                        && should_promote_hidden_branch(
                            state
                                .current_session()
                                .and_then(|session| best_hidden_branch(session).cloned()),
                            current_foreground_branch(state.current_session()).cloned(),
                        )
                    {
                        if let Some(candidate_id) = state
                            .current_session()
                            .and_then(|session| best_hidden_branch(session).map(|branch| branch.id.clone()))
                        {
                            if let Ok(write) =
                                state.promote_branch_to_foreground(&candidate_id, false, None)
                            {
                                persist_write(tx.clone(), store.clone(), write);
                            }
                        }
                    }
                }
            }
            if let Some(branch_id) = finished_branch_id {
                if let Some(session_snapshot) = state.current_session().cloned() {
                    if let Some((branch_id, hidden)) = session_snapshot
                        .proof
                        .branches
                        .iter()
                        .find(|branch| branch.id == branch_id)
                        .map(|branch| (branch.id.clone(), branch.hidden))
                    {
                        if session_snapshot
                            .proof
                            .branches
                            .iter()
                            .find(|branch| branch.id == branch_id)
                            .map(|branch| !branch.lean_snippet.trim().is_empty())
                            .unwrap_or(false)
                        {
                            start_branch_verification(
                                tx.clone(),
                                store.clone(),
                                session_snapshot,
                                branch_id.clone(),
                                !hidden,
                            );
                        } else if state
                            .current_session()
                            .map(|session| session.proof.is_autonomous_running)
                            .unwrap_or(false)
                        {
                            let _ = tx.send(AppEvent::AutonomousTick);
                        }
                    }
                }
            }
        }

        // Flush completed turns to terminal scrollback (enables native scrollbar).
        // A "completed turn" is a user message followed by a finished assistant response.
        // We flush in pairs: if we have entries [user, assistant, user, assistant, user]
        // and assistant is done, we can flush the first 4 (2 complete turns).
        if !state.turn_in_flight {
            if let Some(session) = state.current_session() {
                let transcript_len = session.transcript.len();
                // Flush all entries except the last one (keep it in viewport
                // so the user sees the most recent response).
                let flushable = transcript_len.saturating_sub(1);
                if flushable > state.flushed_turn_count {
                    let entries_to_flush: Vec<_> = session.transcript
                        [state.flushed_turn_count..flushable]
                        .to_vec();
                    let mut lines = Vec::new();
                    for entry in &entries_to_flush {
                        lines.extend(openproof_tui::render_entry(entry));
                    }
                    if !lines.is_empty() {
                        let _ = openproof_tui::insert_history::insert_history_lines(
                            terminal, lines,
                        );
                    }
                    state.flushed_turn_count = flushable;
                }
            }
        }

        terminal.draw(|frame| openproof_tui::draw(frame, state))?;

        if state.should_quit {
            break;
        }

        // Drain all pending terminal events before rendering.
        // First poll blocks up to 16ms; subsequent polls use zero timeout
        // to coalesce rapid inputs (especially scroll) into one frame.
        let mut poll_timeout = Duration::from_millis(16);
        while event::poll(poll_timeout)? {
            poll_timeout = Duration::ZERO;
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if state.overlay.is_some() {
                        handle_overlay_key(key, state, &tx, &store);
                    } else if state.command_mode {
                        handle_command_mode_key(key, state, &tx, &store);
                    } else {
                        let next_event = match key.code {
                            KeyCode::Char('c')
                                if key.modifiers.contains(KeyModifiers::CONTROL) =>
                            {
                                if !state.composer.is_empty() {
                                    state.composer.clear();
                                    state.composer_cursor = 0;
                                    None
                                } else {
                                    Some(AppEvent::Quit)
                                }
                            }
                            KeyCode::Tab => Some(AppEvent::FocusNext),
                            KeyCode::Up if state.has_open_question() => {
                                Some(AppEvent::SelectPrevQuestionOption)
                            }
                            KeyCode::Down if state.has_open_question() => {
                                Some(AppEvent::SelectNextQuestionOption)
                            }
                            KeyCode::Up => Some(match state.focus {
                                FocusPane::Sessions => AppEvent::SelectPrevSession,
                                _ => AppEvent::ScrollTranscriptUp,
                            }),
                            KeyCode::Down => Some(match state.focus {
                                FocusPane::Sessions => AppEvent::SelectNextSession,
                                _ => AppEvent::ScrollTranscriptDown,
                            }),
                            KeyCode::PageUp => Some(AppEvent::ScrollPageUp),
                            KeyCode::PageDown => Some(AppEvent::ScrollPageDown),
                            KeyCode::Left => Some(AppEvent::CursorLeft),
                            KeyCode::Right => Some(AppEvent::CursorRight),
                            KeyCode::Home => Some(AppEvent::CursorHome),
                            KeyCode::End => Some(AppEvent::CursorEnd),
                            KeyCode::Delete => Some(AppEvent::DeleteForward),
                            KeyCode::Backspace => Some(AppEvent::Backspace),
                            KeyCode::Char('a')
                                if key.modifiers.contains(KeyModifiers::CONTROL) =>
                            {
                                Some(AppEvent::CursorHome)
                            }
                            KeyCode::Char('e')
                                if key.modifiers.contains(KeyModifiers::CONTROL) =>
                            {
                                Some(AppEvent::CursorEnd)
                            }
                            KeyCode::Char('u')
                                if key.modifiers.contains(KeyModifiers::CONTROL) =>
                            {
                                Some(AppEvent::ClearToStart)
                            }
                            KeyCode::Char('w')
                                if key.modifiers.contains(KeyModifiers::CONTROL) =>
                            {
                                Some(AppEvent::DeleteWordBackward)
                            }
                            KeyCode::Char('/')
                                if state.composer.is_empty()
                                    && !key.modifiers.contains(KeyModifiers::CONTROL) =>
                            {
                                state.command_mode = true;
                                state.command_buffer.clear();
                                state.command_cursor = 0;
                                state.command_completions =
                                    openproof_core::command_completions("");
                                state.completion_idx = None;
                                None
                            }
                            KeyCode::Char(ch)
                                if !key.modifiers.contains(KeyModifiers::CONTROL) =>
                            {
                                Some(AppEvent::InputChar(ch))
                            }
                            _ => None,
                        };

                        if let Some(next_event) = next_event {
                            if let Some(write) = state.apply(next_event) {
                                persist_write(tx.clone(), store.clone(), write);
                            }
                        } else if matches!(key.code, KeyCode::Enter) {
                            if state.has_open_question()
                                && state.composer.trim().is_empty()
                            {
                                submit_selected_question_option(
                                    tx.clone(),
                                    store.clone(),
                                    state,
                                );
                            } else if let Some(submission) = state.submit_composer() {
                                persist_write(
                                    tx.clone(),
                                    store.clone(),
                                    openproof_core::PendingWrite {
                                        session: submission.session_snapshot.clone(),
                                    },
                                );
                                handle_submission(
                                    tx.clone(),
                                    store.clone(),
                                    state,
                                    submission,
                                );
                            }
                        }
                    }
                }
                Event::Paste(text) => {
                    if let Some(write) = state.apply(AppEvent::Paste(text)) {
                        persist_write(tx.clone(), store.clone(), write);
                    }
                }
                Event::Mouse(mouse) => {
                    use crossterm::event::MouseEventKind;
                    match mouse.kind {
                        MouseEventKind::ScrollUp => {
                            let _ = state.apply(AppEvent::ScrollTranscriptUp);
                        }
                        MouseEventKind::ScrollDown => {
                            let _ = state.apply(AppEvent::ScrollTranscriptDown);
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn submit_selected_question_option(
    tx: mpsc::UnboundedSender<AppEvent>,
    store: AppStore,
    state: &mut AppState,
) {
    let Some(option) = state.selected_question_option().cloned() else {
        emit_local_notice(
            tx,
            state,
            store,
            "Answer Error",
            "No clarification option is currently selectable.".to_string(),
        );
        return;
    };
    let reply_text = if option.formal_target.trim().is_empty() {
        option.label.clone()
    } else {
        option.formal_target.clone()
    };
    if let Some(submitted) = state.submit_text(reply_text) {
        persist_write(
            tx.clone(),
            store.clone(),
            openproof_core::PendingWrite {
                session: submitted.session_snapshot.clone(),
            },
        );
        handle_submission(tx, store, state, submitted);
    }
}

fn persist_write(
    tx: mpsc::UnboundedSender<AppEvent>,
    store: AppStore,
    write: openproof_core::PendingWrite,
) {
    let session_id = write.session.id.clone();
    tokio::spawn(async move {
        let outcome = tokio::task::spawn_blocking(move || store.save_session(&write.session))
            .await
            .ok()
            .and_then(Result::ok);
        match outcome {
            Some(_) => {
                let _ = tx.send(AppEvent::PersistSucceeded(session_id));
            }
            None => {
                let _ = tx.send(AppEvent::PersistFailed("store save failed".to_string()));
            }
        }
    });
}

fn persist_verification_result(
    tx: mpsc::UnboundedSender<AppEvent>,
    store: AppStore,
    session: SessionSnapshot,
    result: openproof_protocol::LeanVerificationSummary,
) {
    tokio::spawn(async move {
        let outcome = tokio::task::spawn_blocking(move || store.record_verification_result(&session, &result))
            .await
            .ok()
            .and_then(Result::ok);
        if outcome.is_none() {
            let _ = tx.send(AppEvent::AppendNotice {
                title: "Verification Store Error".to_string(),
                content: "Could not persist the verification outcome into the native corpus store."
                    .to_string(),
            });
        }
    });
}

fn handle_overlay_key(
    key: event::KeyEvent,
    state: &mut AppState,
    tx: &mpsc::UnboundedSender<AppEvent>,
    store: &AppStore,
) {
    let Some(overlay) = state.overlay.take() else {
        return;
    };
    match overlay {
        openproof_core::Overlay::SessionPicker { mut selected } => match key.code {
            KeyCode::Esc => {
                // Close without action.
            }
            KeyCode::Up => {
                selected = selected.saturating_sub(1);
                state.overlay = Some(openproof_core::Overlay::SessionPicker { selected });
            }
            KeyCode::Down => {
                if selected + 1 < state.sessions.len() {
                    selected += 1;
                }
                state.overlay = Some(openproof_core::Overlay::SessionPicker { selected });
            }
            KeyCode::Enter => {
                if let Some(session) = state.sessions.get(selected) {
                    let id = session.id.clone();
                    match state.switch_session(&id) {
                        Ok(()) => {
                            state.sync_question_selection();
                        }
                        Err(e) => {
                            emit_local_notice(
                                tx.clone(),
                                state,
                                store.clone(),
                                "Resume Error",
                                e,
                            );
                        }
                    }
                }
            }
            _ => {
                // Keep overlay open on unrecognized keys.
                state.overlay = Some(openproof_core::Overlay::SessionPicker { selected });
            }
        },
        openproof_core::Overlay::FocusPicker { items, mut selected } => match key.code {
            KeyCode::Esc => {
                // Close without action.
            }
            KeyCode::Up => {
                selected = selected.saturating_sub(1);
                state.overlay =
                    Some(openproof_core::Overlay::FocusPicker { items, selected });
            }
            KeyCode::Down => {
                if selected + 1 < items.len() {
                    selected += 1;
                }
                state.overlay =
                    Some(openproof_core::Overlay::FocusPicker { items, selected });
            }
            KeyCode::Enter => {
                if let Some((id, _label, _kind)) = items.get(selected) {
                    match state.focus_target(Some(id)) {
                        Ok(Some(write)) => {
                            persist_write(tx.clone(), store.clone(), write);
                            emit_local_notice(
                                tx.clone(),
                                state,
                                store.clone(),
                                "Focus",
                                format!("Focused {id}."),
                            );
                        }
                        Ok(None) => {}
                        Err(e) => {
                            emit_local_notice(
                                tx.clone(),
                                state,
                                store.clone(),
                                "Focus Error",
                                e,
                            );
                        }
                    }
                }
            }
            _ => {
                state.overlay =
                    Some(openproof_core::Overlay::FocusPicker { items, selected });
            }
        },
    }
}

fn handle_command_mode_key(
    key: event::KeyEvent,
    state: &mut AppState,
    tx: &mpsc::UnboundedSender<AppEvent>,
    store: &AppStore,
) {
    match key.code {
        KeyCode::Esc => {
            state.command_mode = false;
            state.command_buffer.clear();
            state.command_cursor = 0;
            state.command_completions.clear();
            state.completion_idx = None;
        }
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.command_mode = false;
            state.command_buffer.clear();
            state.command_cursor = 0;
            state.command_completions.clear();
            state.completion_idx = None;
        }
        KeyCode::Enter => {
            let buffer = state.command_buffer.clone();
            state.command_mode = false;
            state.command_buffer.clear();
            state.command_cursor = 0;
            state.command_completions.clear();
            state.completion_idx = None;
            if !buffer.is_empty() {
                // Submit as a slash command through the normal path.
                let text = format!("/{buffer}");
                if let Some(submission) = state.submit_text(text) {
                    persist_write(
                        tx.clone(),
                        store.clone(),
                        openproof_core::PendingWrite {
                            session: submission.session_snapshot.clone(),
                        },
                    );
                    handle_submission(tx.clone(), store.clone(), state, submission);
                }
            }
        }
        KeyCode::Tab => {
            // Cycle to next completion.
            if state.command_completions.is_empty() {
                state.command_completions =
                    openproof_core::command_completions(&state.command_buffer);
                state.completion_idx = None;
            }
            if !state.command_completions.is_empty() {
                let idx = match state.completion_idx {
                    Some(i) => (i + 1) % state.command_completions.len(),
                    None => 0,
                };
                state.completion_idx = Some(idx);
                state.command_buffer = state.command_completions[idx].clone();
                state.command_cursor = state.command_buffer.len();
            }
        }
        KeyCode::BackTab => {
            // Cycle to previous completion.
            if state.command_completions.is_empty() {
                state.command_completions =
                    openproof_core::command_completions(&state.command_buffer);
                state.completion_idx = None;
            }
            if !state.command_completions.is_empty() {
                let idx = match state.completion_idx {
                    Some(0) | None => state.command_completions.len() - 1,
                    Some(i) => i - 1,
                };
                state.completion_idx = Some(idx);
                state.command_buffer = state.command_completions[idx].clone();
                state.command_cursor = state.command_buffer.len();
            }
        }
        // Ctrl shortcuts
        KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.command_cursor = 0;
        }
        KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.command_cursor = state.command_buffer.len();
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.command_buffer.drain(..state.command_cursor);
            state.command_cursor = 0;
            state.command_completions =
                openproof_core::command_completions(&state.command_buffer);
            state.completion_idx = None;
        }
        KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if state.command_cursor > 0 {
                let new_pos = openproof_core::delete_word_backward_pos(
                    &state.command_buffer,
                    state.command_cursor,
                );
                state.command_buffer.drain(new_pos..state.command_cursor);
                state.command_cursor = new_pos;
                state.command_completions =
                    openproof_core::command_completions(&state.command_buffer);
                state.completion_idx = None;
            }
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.command_buffer.insert(state.command_cursor, c);
            state.command_cursor += c.len_utf8();
            state.command_completions =
                openproof_core::command_completions(&state.command_buffer);
            state.completion_idx = None;
        }
        KeyCode::Backspace => {
            if state.command_cursor > 0 {
                let prev = state.command_buffer[..state.command_cursor]
                    .char_indices()
                    .next_back()
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                state.command_buffer.remove(prev);
                state.command_cursor = prev;
                state.command_completions =
                    openproof_core::command_completions(&state.command_buffer);
                state.completion_idx = None;
            }
        }
        KeyCode::Left => {
            if state.command_cursor > 0 {
                state.command_cursor = state.command_buffer[..state.command_cursor]
                    .char_indices()
                    .next_back()
                    .map(|(i, _)| i)
                    .unwrap_or(0);
            }
        }
        KeyCode::Right => {
            if state.command_cursor < state.command_buffer.len() {
                state.command_cursor = state.command_buffer[state.command_cursor..]
                    .char_indices()
                    .nth(1)
                    .map(|(i, _)| state.command_cursor + i)
                    .unwrap_or(state.command_buffer.len());
            }
        }
        KeyCode::Delete => {
            if state.command_cursor < state.command_buffer.len() {
                state.command_buffer.remove(state.command_cursor);
                state.command_completions =
                    openproof_core::command_completions(&state.command_buffer);
                state.completion_idx = None;
            }
        }
        KeyCode::Home => {
            state.command_cursor = 0;
        }
        KeyCode::End => {
            state.command_cursor = state.command_buffer.len();
        }
        _ => {}
    }
}

fn handle_submission(
    tx: mpsc::UnboundedSender<AppEvent>,
    store: AppStore,
    state: &mut AppState,
    submission: SubmittedInput,
) {
    if submission.raw_text.trim_start().starts_with('/') {
        apply_local_command(tx, state, store, submission);
        return;
    }

    if state.turn_in_flight {
        let _ = tx.send(AppEvent::AppendNotice {
            title: "Busy".to_string(),
            content: "A model turn is already running. Wait for it to finish before submitting another prompt.".to_string(),
        });
        return;
    }

    let _ = state.apply(AppEvent::TurnStarted);
    let session_snapshot = submission.session_snapshot.clone();
    let tx_model = tx.clone();
    let store_for_model = store.clone();
    tokio::spawn(async move {
        let messages = build_turn_messages_with_retrieval(&store_for_model, Some(&session_snapshot)).await;
        let result = run_codex_turn(CodexTurnRequest {
            session_id: &submission.session_id,
            messages: &messages,
            model: "gpt-5.4",
            reasoning_effort: "high",
        })
        .await;

        match result {
            Ok(text) => {
                let content = if text.trim().is_empty() {
                    "The model returned no visible text.".to_string()
                } else {
                    text
                };
                let _ = tx_model.send(AppEvent::AppendAssistant(content));
            }
            Err(error) => {
                let _ = tx_model.send(AppEvent::AppendNotice {
                    title: "Assistant Error".to_string(),
                    content: error.to_string(),
                });
            }
        }
        let _ = tx_model.send(AppEvent::TurnFinished);
    });
}

fn start_agent_branch_turn(
    tx: mpsc::UnboundedSender<AppEvent>,
    store: AppStore,
    role: AgentRole,
    title: String,
    branch_id: String,
    _task_id: String,
    session_snapshot: SessionSnapshot,
) {
    tokio::spawn(async move {
        let messages = build_branch_turn_messages(&store, &session_snapshot, role, &title, &branch_id).await;
        let result = run_codex_turn(CodexTurnRequest {
            session_id: &branch_id,
            messages: &messages,
            model: "gpt-5.4",
            reasoning_effort: "high",
        })
        .await;

        match result {
            Ok(text) => {
                let content = if text.trim().is_empty() {
                    "The model returned no visible text.".to_string()
                } else {
                    text
                };
                let summary = summarize_branch_output(&content);
                let _ = tx.send(AppEvent::AppendBranchAssistant {
                    branch_id: branch_id.clone(),
                    content,
                });
                let _ = tx.send(AppEvent::FinishBranch {
                    branch_id,
                    status: openproof_protocol::AgentStatus::Done,
                    summary,
                    output: String::new(),
                });
            }
            Err(error) => {
                let message = error.to_string();
                let _ = tx.send(AppEvent::FinishBranch {
                    branch_id,
                    status: openproof_protocol::AgentStatus::Error,
                    summary: format!("Branch failed: {}", truncate(&message, 160)),
                    output: message,
                });
            }
        }
    });
}

fn start_branch_verification(
    tx: mpsc::UnboundedSender<AppEvent>,
    store: AppStore,
    session_snapshot: SessionSnapshot,
    branch_id: String,
    promote: bool,
) {
    let Some((verification_session, focus_node_id)) =
        build_branch_verification_session(&session_snapshot, &branch_id)
    else {
        let _ = tx.send(AppEvent::AppendNotice {
            title: "Verify Error".to_string(),
            content: format!("Branch {branch_id} has no Lean candidate to verify."),
        });
        return;
    };

    let _ = tx.send(AppEvent::LeanVerifyStarted);
    let project_dir = resolve_lean_project_dir();
    tokio::spawn(async move {
        let verification_clone = verification_session.clone();
        let result = tokio::task::spawn_blocking(move || verify_active_node(&project_dir, &verification_clone))
            .await
            .ok()
            .and_then(Result::ok);
        match result {
            Some(result) => {
                let persist_store = store.clone();
                let persist_session = verification_session.clone();
                let persist_result = result.clone();
                let persist_tx = tx.clone();
                tokio::spawn(async move {
                    let persisted = tokio::task::spawn_blocking(move || {
                        persist_store.record_verification_result(&persist_session, &persist_result)
                    })
                    .await
                    .ok()
                    .and_then(Result::ok);
                    if persisted.is_none() {
                        let _ = persist_tx.send(AppEvent::AppendNotice {
                            title: "Verification Store Error".to_string(),
                            content: "Could not persist the branch verification outcome.".to_string(),
                        });
                    }
                });
                let _ = tx.send(AppEvent::BranchVerifyFinished {
                    branch_id,
                    focus_node_id,
                    promote,
                    result,
                });
            }
            None => {
                let _ = tx.send(AppEvent::AppendNotice {
                    title: "Verify Error".to_string(),
                    content: format!("Lean verification crashed for branch {branch_id}."),
                });
            }
        }
    });
}

fn build_branch_verification_session(
    session: &SessionSnapshot,
    branch_id: &str,
) -> Option<(SessionSnapshot, Option<String>)> {
    let branch = session
        .proof
        .branches
        .iter()
        .find(|branch| branch.id == branch_id)?;
    if branch.lean_snippet.trim().is_empty() {
        return None;
    }
    let focus_node_id = branch
        .focus_node_id
        .clone()
        .or_else(|| session.proof.active_node_id.clone())?;
    let mut verification_session = session.clone();
    verification_session.proof.active_node_id = Some(focus_node_id.clone());
    if let Some(node) = verification_session
        .proof
        .nodes
        .iter_mut()
        .find(|node| node.id == focus_node_id)
    {
        node.content = branch.lean_snippet.clone();
    } else {
        return None;
    }
    Some((verification_session, Some(focus_node_id)))
}

fn persist_current_session(
    tx: mpsc::UnboundedSender<AppEvent>,
    store: AppStore,
    state: &mut AppState,
    status: impl Into<String>,
) {
    let Some(session) = state.current_session().cloned() else {
        return;
    };
    state.pending_writes += 1;
    state.status = status.into();
    persist_write(tx, store, openproof_core::PendingWrite { session });
}

fn best_hidden_branch(session: &SessionSnapshot) -> Option<&openproof_protocol::ProofBranch> {
    session
        .proof
        .branches
        .iter()
        .filter(|branch| branch.hidden)
        .max_by(|left, right| {
            left.score
                .partial_cmp(&right.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.updated_at.cmp(&right.updated_at))
        })
}

fn current_foreground_branch(session: Option<&SessionSnapshot>) -> Option<&openproof_protocol::ProofBranch> {
    let session = session?;
    session
        .proof
        .active_foreground_branch_id
        .as_deref()
        .and_then(|branch_id| session.proof.branches.iter().find(|branch| branch.id == branch_id))
}

fn should_promote_hidden_branch(
    candidate: Option<openproof_protocol::ProofBranch>,
    current: Option<openproof_protocol::ProofBranch>,
) -> bool {
    let Some(candidate) = candidate else {
        return false;
    };
    let Some(current) = current else {
        return true;
    };
    if candidate.score >= 100.0 && current.score < 100.0 {
        return true;
    }
    if candidate.score > current.score + 12.0 {
        return true;
    }
    let candidate_has_diag = candidate
        .latest_diagnostics
        .as_ref()
        .map(|item| !item.trim().is_empty())
        .unwrap_or(false)
        || !candidate.last_lean_diagnostic.trim().is_empty();
    let current_has_diag = current
        .latest_diagnostics
        .as_ref()
        .map(|item| !item.trim().is_empty())
        .unwrap_or(false)
        || !current.last_lean_diagnostic.trim().is_empty();
    !candidate_has_diag && current_has_diag
}

fn autonomous_stop_reason(session: &SessionSnapshot) -> Option<String> {
    if session.proof.pending_question.is_some() || session.proof.awaiting_clarification {
        return Some("Autonomous loop paused for clarification.".to_string());
    }
    if session.proof.phase == "done" {
        return Some("Autonomous loop completed the current proof run.".to_string());
    }
    if session.proof.phase == "blocked" {
        return Some("Autonomous loop paused on a blocker.".to_string());
    }
    if session.proof.accepted_target.is_none() && session.proof.formal_target.is_none() {
        return Some("Set or accept a formal target before running autonomous search.".to_string());
    }
    let all_finished = !session.proof.branches.is_empty()
        && session
            .proof
            .branches
            .iter()
            .all(|branch| branch.status != AgentStatus::Running);
    let all_stalled = all_finished
        && session.proof.autonomous_iteration_count >= 6
        && session
            .proof
            .branches
            .iter()
            .all(|branch| matches!(branch.status, AgentStatus::Blocked | AgentStatus::Done | AgentStatus::Error));
    if all_stalled {
        return Some("Autonomous loop paused after low-progress iterations.".to_string());
    }
    None
}

fn ensure_hidden_agent_branch(
    tx: mpsc::UnboundedSender<AppEvent>,
    store: AppStore,
    state: &mut AppState,
    role: AgentRole,
    title: &str,
    description: &str,
) -> Result<(String, SessionSnapshot), String> {
    let existing_id = state.current_session().and_then(|session| {
        session
            .proof
            .branches
            .iter()
            .filter(|branch| branch.hidden && branch.role == role)
            .max_by(|left, right| left.updated_at.cmp(&right.updated_at))
            .map(|branch| branch.id.clone())
    });

    if let Some(branch_id) = existing_id {
        let now = chrono::Utc::now().to_rfc3339();
        if let Some(session) = state.current_session_mut() {
            if let Some(branch) = session
                .proof
                .branches
                .iter_mut()
                .find(|branch| branch.id == branch_id)
            {
                branch.title = title.to_string();
                branch.hidden = true;
                branch.branch_kind = format!("{}_hidden", agent_role_label(role));
                branch.status = AgentStatus::Running;
                branch.queue_state = BranchQueueState::Running;
                branch.phase = Some(branch_phase_for_role(role).to_string());
                branch.goal_summary = description.to_string();
                branch.search_status = format!("{} branch restarted", agent_role_label(role));
                branch.progress_kind = Some(
                    match role {
                        AgentRole::Planner => "planning",
                        AgentRole::Retriever => "retrieving",
                        AgentRole::Repairer => "repairing",
                        AgentRole::Prover => "candidate",
                        AgentRole::Critic => "blocked",
                    }
                    .to_string(),
                );
                branch.summary = description.to_string();
                branch.updated_at = now.clone();
            }
            session.updated_at = now;
        }
        persist_current_session(tx, store, state, format!("Restarted {} branch.", agent_role_label(role)));
        let snapshot = state
            .current_session()
            .cloned()
            .ok_or_else(|| "No active session.".to_string())?;
        return Ok((branch_id, snapshot));
    }

    let (write, branch_id, _task_id) = state.spawn_agent_branch(role, title, description, true)?;
    let snapshot = write.session.clone();
    persist_write(tx, store, write);
    Ok((branch_id, snapshot))
}

fn refresh_retrieval_branch(
    tx: mpsc::UnboundedSender<AppEvent>,
    store: AppStore,
    state: &mut AppState,
) -> Result<String, String> {
    let Some(session) = state.current_session().cloned() else {
        return Err("No active session.".to_string());
    };
    let query = session
        .proof
        .active_node_id
        .as_deref()
        .and_then(|node_id| session.proof.nodes.iter().find(|node| node.id == node_id))
        .map(|node| node.statement.clone())
        .or_else(|| session.proof.accepted_target.clone())
        .or_else(|| session.proof.formal_target.clone())
        .unwrap_or_default();
    if query.trim().is_empty() {
        return Ok("No target is ready for verified retrieval yet.".to_string());
    }
    let hits = store
        .search_verified_corpus(&query, 6)
        .map_err(|error| error.to_string())?;
    let summary = if hits.is_empty() {
        "No strong verified references found for the current target.".to_string()
    } else {
        format!(
            "Retrieved {} verified references. Best hit: {}.",
            hits.len(),
            hits.first().map(|item| item.0.clone()).unwrap_or_else(|| "n/a".to_string())
        )
    };

    let branch_id = state.current_session().and_then(|current| {
        current
            .proof
            .branches
            .iter()
            .filter(|branch| branch.hidden && branch.role == AgentRole::Retriever)
            .max_by(|left, right| left.updated_at.cmp(&right.updated_at))
            .map(|branch| branch.id.clone())
    });
    let branch_id = if let Some(branch_id) = branch_id {
        let now = chrono::Utc::now().to_rfc3339();
        if let Some(current) = state.current_session_mut() {
            if let Some(branch) = current
                .proof
                .branches
                .iter_mut()
                .find(|branch| branch.id == branch_id)
            {
                branch.hidden = true;
                branch.branch_kind = "retriever_hidden".to_string();
                branch.status = AgentStatus::Done;
                branch.queue_state = BranchQueueState::Done;
                branch.phase = Some("retrieving".to_string());
                branch.goal_summary = query.clone();
                branch.score = if hits.is_empty() {
                    0.0
                } else {
                    18.0 + hits.len() as f32 * 3.0
                };
                branch.progress_kind = Some("retrieving".to_string());
                branch.search_status = summary.clone();
                branch.summary = hits
                    .iter()
                    .take(3)
                    .map(|(label, statement, visibility)| format!("{label} [{visibility}] :: {statement}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                branch.updated_at = now.clone();
            }
            current.updated_at = now;
            current.proof.active_retrieval_summary = Some(summary.clone());
        }
        persist_current_session(
            tx.clone(),
            store.clone(),
            state,
            "Updated verified retrieval branch.".to_string(),
        );
        branch_id
    } else {
        let (branch_id, snapshot) = ensure_hidden_agent_branch(
            tx.clone(),
            store.clone(),
            state,
            AgentRole::Retriever,
            "Verified retrieval",
            &query,
        )?;
        if let Some(current) = state.current_session_mut() {
            if let Some(branch) = current
                .proof
                .branches
                .iter_mut()
                .find(|branch| branch.id == branch_id)
            {
                branch.status = AgentStatus::Done;
                branch.queue_state = BranchQueueState::Done;
                branch.phase = Some("retrieving".to_string());
                branch.score = if hits.is_empty() {
                    0.0
                } else {
                    18.0 + hits.len() as f32 * 3.0
                };
                branch.progress_kind = Some("retrieving".to_string());
                branch.search_status = summary.clone();
                branch.summary = hits
                    .iter()
                    .take(3)
                    .map(|(label, statement, visibility)| format!("{label} [{visibility}] :: {statement}"))
                    .collect::<Vec<_>>()
                    .join("\n");
            }
            current.proof.active_retrieval_summary = Some(summary.clone());
        }
        let _ = snapshot;
        persist_current_session(
            tx.clone(),
            store.clone(),
            state,
            "Recorded verified retrieval hits.".to_string(),
        );
        branch_id
    };

    if let Ok(write) = state.refresh_hidden_search_state(Some(Some(summary.clone()))) {
        persist_write(tx, store, write);
    }
    Ok(format!("{} [{}]", summary, branch_id))
}

fn schedule_autonomous_tick(
    tx: mpsc::UnboundedSender<AppEvent>,
    store: AppStore,
    state: &mut AppState,
) {
    let Some(session) = state.current_session().cloned() else {
        return;
    };
    if !session.proof.is_autonomous_running {
        return;
    }
    if state.turn_in_flight || state.verification_in_flight {
        return;
    }
    if session
        .proof
        .branches
        .iter()
        .any(|branch| branch.status == AgentStatus::Running)
    {
        return;
    }
    if let Some(reason) = autonomous_stop_reason(&session) {
        if let Ok(write) = state.set_autonomous_run_state(AutonomousRunPatch {
            is_autonomous_running: Some(false),
            autonomous_pause_reason: Some(Some(reason.clone())),
            autonomous_stop_reason: Some(if session.proof.phase == "done" {
                Some(reason.clone())
            } else {
                None
            }),
            ..AutonomousRunPatch::default()
        }) {
            persist_write(tx, store, write);
        }
        return;
    }
    let _ = run_autonomous_step(tx, store, state);
}

fn apply_local_command(
    tx: mpsc::UnboundedSender<AppEvent>,
    state: &mut AppState,
    store: AppStore,
    submission: SubmittedInput,
) {
    let trimmed = submission.raw_text.trim();
    let mut parts = trimmed.splitn(2, ' ');
    let command = parts.next().unwrap_or("");
    let arg_text = parts.next().unwrap_or("").trim();
    match command {
        "/help" => {
            emit_local_notice(
                tx,
                state,
                store,
                "Help",
                [
                    "/help",
                    "/status",
                    "/new <title>",
                    "/clear [title]",
                    "/resume <session-id>",
                    "/nodes",
                    "/branches",
                    "/agents",
                    "/tasks",
                    "/focus <branch-id|node-id|clear>",
                    "/agent spawn <role> <task>",
                    "/proof",
                    "/paper",
                    "/questions",
                    "/answer <option-id|text>",
                    "/instructions",
                    "/memory",
                    "/remember <text>",
                    "/remember global <text>",
                    "/share [local|community|private]",
                    "/share overlay [on|off]",
                    "/corpus status|search <query>|ingest|recluster",
                    "/sync status|enable|disable|drain",
                    "/export paper|tex|lean|all",
                    "/autonomous status|start|stop|step",
                    "/theorem <label> :: <statement>",
                    "/lemma <label> :: <statement>",
                    "/verify",
                    "/login",
                    "/dashboard",
                    "/sessions",
                    "Tab focuses panes. Enter sends. q quits.",
                ]
                .join("\n"),
            );
        }
        "/status" => {
            emit_local_notice(tx, state, store, "Status", state.status_report());
        }
        "/branches" => {
            emit_local_notice(tx, state, store, "Branches", state.branches_report());
        }
        "/agents" => {
            emit_local_notice(tx, state, store, "Agents", state.agents_report());
        }
        "/tasks" => {
            emit_local_notice(tx, state, store, "Tasks", state.tasks_report());
        }
        "/new" => {
            let write = state.create_session(if arg_text.is_empty() { None } else { Some(arg_text) });
            persist_write(tx.clone(), store.clone(), write);
            emit_local_notice(
                tx,
                state,
                store,
                "Session",
                format!(
                    "Started new session: {}.",
                    state
                        .current_session()
                        .map(|session| session.title.clone())
                        .unwrap_or_else(|| "OpenProof Rust Session".to_string())
                ),
            );
        }
        "/clear" => {
            let write = state.create_session(if arg_text.is_empty() { None } else { Some(arg_text) });
            persist_write(tx.clone(), store.clone(), write);
            emit_local_notice(
                tx,
                state,
                store,
                "Session",
                format!(
                    "Cleared into new session: {}.",
                    state
                        .current_session()
                        .map(|session| session.title.clone())
                        .unwrap_or_else(|| "OpenProof Rust Session".to_string())
                ),
            );
        }
        "/resume" => {
            if arg_text.is_empty() {
                // Open interactive session picker.
                state.overlay = Some(openproof_core::Overlay::SessionPicker {
                    selected: state.selected_session,
                });
            } else {
                match state.switch_session(arg_text) {
                    Ok(()) => emit_local_notice(
                        tx,
                        state,
                        store,
                        "Session",
                        format!(
                            "Resumed {}.",
                            state
                                .current_session()
                                .map(|session| session.title.clone())
                                .unwrap_or_else(|| arg_text.to_string())
                        ),
                    ),
                    Err(error) => {
                        emit_local_notice(tx, state, store, "Session Error", error)
                    }
                }
            }
        }
        "/nodes" => {
            emit_local_notice(tx, state, store, "Proof Nodes", state.proof_nodes_report());
        }
        "/focus" => {
            if arg_text.is_empty() {
                // Open interactive focus picker.
                let items = openproof_core::build_focus_items(state);
                if items.is_empty() {
                    emit_local_notice(
                        tx,
                        state,
                        store,
                        "Focus",
                        "No focusable targets (no nodes or branches).".to_string(),
                    );
                } else {
                    state.overlay = Some(openproof_core::Overlay::FocusPicker {
                        items,
                        selected: 0,
                    });
                }
            } else if arg_text == "clear" {
                match state.focus_target(None) {
                    Ok(Some(write)) => {
                        persist_write(tx.clone(), store.clone(), write);
                        emit_local_notice(
                            tx,
                            state,
                            store,
                            "Focus",
                            "Cleared active proof focus.".to_string(),
                        );
                    }
                    Ok(None) => {}
                    Err(error) => emit_local_notice(tx, state, store, "Focus Error", error),
                }
            } else {
                match state.focus_target(Some(arg_text)) {
                    Ok(Some(write)) => {
                        persist_write(tx.clone(), store.clone(), write);
                        emit_local_notice(
                            tx,
                            state,
                            store,
                            "Focus",
                            format!("Focused {arg_text}."),
                        );
                    }
                    Ok(None) => {}
                    Err(error) => emit_local_notice(tx, state, store, "Focus Error", error),
                }
            }
        }
        "/agent" => {
            let parts = arg_text.split_whitespace().collect::<Vec<_>>();
            if parts.first().copied() != Some("spawn") || parts.len() < 3 {
                emit_local_notice(
                    tx,
                    state,
                    store,
                    "Agent Usage",
                    "Usage: /agent spawn <planner|prover|repairer|retriever|critic> <task>"
                        .to_string(),
                );
                return;
            }
            let Some(role) = parse_agent_role(parts[1]) else {
                emit_local_notice(
                    tx,
                    state,
                    store,
                    "Agent Usage",
                    "Unknown agent role. Use planner|prover|repairer|retriever|critic.".to_string(),
                );
                return;
            };
            let title = parts[2..].join(" ");
            match state.spawn_agent_branch(role, &title, &title, false) {
                Ok((write, branch_id, task_id)) => {
                    let session_snapshot = write.session.clone();
                    persist_write(tx.clone(), store.clone(), write);
                    start_agent_branch_turn(
                        tx.clone(),
                        store.clone(),
                        role,
                        title.clone(),
                        branch_id.clone(),
                        task_id.clone(),
                        session_snapshot,
                    );
                    emit_local_notice(
                        tx,
                        state,
                        store,
                        "Agent",
                        format!(
                            "Started {} branch {} with task {}.",
                            agent_role_label(role),
                            branch_id,
                            task_id
                        ),
                    );
                }
                Err(error) => emit_local_notice(tx, state, store, "Agent Error", error),
            }
        }
        "/proof" => {
            let report = state.proof_status_report();
            emit_local_notice(tx, state, store, "Proof State", report);
        }
        "/paper" => {
            emit_local_notice(tx, state, store, "Paper", state.paper_report());
        }
        "/questions" => {
            emit_local_notice(tx, state, store, "Questions", state.pending_question_report());
        }
        "/instructions" => {
            let context = load_prompt_context();
            let content = if context.instructions.trim().is_empty() {
                "No AGENTS.md instructions loaded.".to_string()
            } else {
                context.instructions
            };
            emit_local_notice(tx, state, store, "Instructions", content);
        }
        "/memory" => {
            let context = load_prompt_context();
            let content = if context.memory.trim().is_empty() {
                "No memory recorded yet.".to_string()
            } else {
                context.memory
            };
            emit_local_notice(tx, state, store, "Memory", content);
        }
        "/remember" => {
            if arg_text.is_empty() {
                emit_local_notice(
                    tx,
                    state,
                    store,
                    "Remember Usage",
                    "Usage: /remember <text> or /remember global <text>".to_string(),
                );
                return;
            }
            let context = load_prompt_context();
            let (target_path, text) = if let Some(rest) = arg_text.strip_prefix("global ") {
                (context.global_memory_path, rest.trim())
            } else {
                (context.workspace_memory_path, arg_text.trim())
            };
            if text.is_empty() {
                emit_local_notice(
                    tx,
                    state,
                    store,
                    "Remember Usage",
                    "Usage: /remember <text> or /remember global <text>".to_string(),
                );
                return;
            }
            match append_memory_entry(&target_path, text) {
                Ok(line) => emit_local_notice(tx, state, store, "Memory Saved", line),
                Err(error) => emit_local_notice(tx, state, store, "Memory Error", error.to_string()),
            }
        }
        "/share" => {
            if arg_text.is_empty() {
                let content = state
                    .current_session()
                    .map(|session| {
                        [
                            format!("Share mode: {}", share_mode_label(session.cloud.share_mode)),
                            format!("Sync enabled: {}", if session.cloud.sync_enabled { "yes" } else { "no" }),
                            format!(
                                "Private overlay community: {}",
                                if session.cloud.private_overlay_community { "on" } else { "off" }
                            ),
                            format!(
                                "Last sync: {}",
                                session.cloud.last_sync_at.clone().unwrap_or_else(|| "never".to_string())
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
                    "Usage: /share [local|community|private] or /share overlay [on|off]"
                        .to_string(),
                ),
            }
        }
        "/corpus" => {
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
                                summary.latest_updated_at.unwrap_or_else(|| "never".to_string())
                            ),
                        ]
                        .join("\n"),
                    ),
                    Err(error) => emit_local_notice(tx, state, store, "Corpus Error", error.to_string()),
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
                        Err(error) => emit_local_notice(tx, state, store, "Corpus Error", error.to_string()),
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
                                        .map(|(package, count)| format!("{package}: {count} declarations"))
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
        "/sync" => {
            let subcommand = if arg_text.is_empty() { "status" } else { arg_text };
            match subcommand {
                "status" => match store.get_sync_summary() {
                    Ok(summary) => {
                        let content = state
                            .current_session()
                            .map(|session| {
                                [
                                    format!("Share mode: {}", share_mode_label(session.cloud.share_mode)),
                                    format!("Sync enabled: {}", if session.cloud.sync_enabled { "yes" } else { "no" }),
                                    format!("Pending jobs: {}", summary.pending_count),
                                    format!("Failed jobs: {}", summary.failed_count),
                                    format!("Sent jobs: {}", summary.sent_count),
                                    format!(
                                        "Last sync: {}",
                                        session.cloud.last_sync_at.clone().unwrap_or_else(|| "never".to_string())
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
                        emit_local_notice(tx, state, store, "Sync", "Enabled sync for the current session.".to_string());
                    }
                    Err(error) => emit_local_notice(tx, state, store, "Sync Error", error),
                },
                "disable" => match state.set_sync_enabled(false) {
                    Ok(write) => {
                        persist_write(tx.clone(), store.clone(), write);
                        emit_local_notice(tx, state, store, "Sync", "Disabled sync for the current session.".to_string());
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
        "/export" => {
            let target = if arg_text.is_empty() { "all" } else { arg_text };
            match export_session_artifacts(state.current_session(), target) {
                Ok(paths) => emit_local_notice(
                    tx,
                    state,
                    store,
                    "Export",
                    if paths.is_empty() {
                        "Nothing was exported.".to_string()
                    } else {
                        paths.join("\n")
                    },
                ),
                Err(error) => emit_local_notice(tx, state, store, "Export Error", error.to_string()),
            }
        }
        "/autonomous" => {
            let subcommand = if arg_text.is_empty() { "status" } else { arg_text };
            match subcommand {
                "status" => {
                    let content = state
                        .current_session()
                        .map(|session| {
                            [
                                format!("Phase: {}", session.proof.phase),
                                format!(
                                    "Running: {}",
                                    if session.proof.is_autonomous_running { "yes" } else { "no" }
                                ),
                                format!(
                                    "Accepted target: {}",
                                    session
                                        .proof
                                        .accepted_target
                                        .clone()
                                        .or(session.proof.formal_target.clone())
                                        .unwrap_or_else(|| "none".to_string())
                                ),
                                format!("Branches: {}", session.proof.branches.len()),
                                format!("Hidden branches: {}", session.proof.hidden_branch_count),
                                format!(
                                    "Best hidden: {}",
                                    session
                                        .proof
                                        .hidden_best_branch_id
                                        .clone()
                                        .unwrap_or_else(|| "none".to_string())
                                ),
                                format!("Iteration: {}", session.proof.autonomous_iteration_count),
                                format!(
                                    "Started: {}",
                                    session
                                        .proof
                                        .autonomous_started_at
                                        .clone()
                                        .unwrap_or_else(|| "never".to_string())
                                ),
                                format!(
                                    "Last progress: {}",
                                    session
                                        .proof
                                        .autonomous_last_progress_at
                                        .clone()
                                        .unwrap_or_else(|| "never".to_string())
                                ),
                                format!(
                                    "Pause reason: {}",
                                    session
                                        .proof
                                        .autonomous_pause_reason
                                        .clone()
                                        .unwrap_or_else(|| "none".to_string())
                                ),
                                format!(
                                    "Stop reason: {}",
                                    session
                                        .proof
                                        .autonomous_stop_reason
                                        .clone()
                                        .unwrap_or_else(|| "none".to_string())
                                ),
                                format!(
                                    "Foreground branch: {}",
                                    session
                                        .proof
                                        .active_foreground_branch_id
                                        .clone()
                                        .unwrap_or_else(|| "none".to_string())
                                ),
                                format!(
                                    "Strategy: {}",
                                    session
                                        .proof
                                        .strategy_summary
                                        .clone()
                                        .unwrap_or_else(|| "none".to_string())
                                ),
                            ]
                            .join("\n")
                        })
                        .unwrap_or_else(|| "No active session.".to_string());
                    emit_local_notice(tx, state, store, "Autonomous", content);
                }
                "start" => {
                    let session = match state.current_session().cloned() {
                        Some(session) => session,
                        None => {
                            emit_local_notice(
                                tx,
                                state,
                                store,
                                "Autonomous Error",
                                "No active session.".to_string(),
                            );
                            return;
                        }
                    };
                    if let Some(reason) = autonomous_stop_reason(&session).filter(|reason| {
                        !reason.contains("completed the current proof run")
                    }) {
                        emit_local_notice(tx, state, store, "Autonomous Error", reason);
                        return;
                    }
                    let now = chrono::Utc::now().to_rfc3339();
                    match state.set_autonomous_run_state(AutonomousRunPatch {
                        is_autonomous_running: Some(true),
                        autonomous_started_at: Some(Some(
                            session
                                .proof
                                .autonomous_started_at
                                .clone()
                                .unwrap_or(now.clone()),
                        )),
                        autonomous_last_progress_at: Some(session.proof.autonomous_last_progress_at.clone().or(Some(now))),
                        autonomous_pause_reason: Some(None),
                        autonomous_stop_reason: Some(None),
                        ..AutonomousRunPatch::default()
                    }) {
                        Ok(write) => {
                            persist_write(tx.clone(), store.clone(), write);
                            let _ = tx.send(AppEvent::AutonomousTick);
                            emit_local_notice(
                                tx,
                                state,
                                store,
                                "Autonomous",
                                "Autonomous proof loop started.".to_string(),
                            );
                        }
                        Err(error) => emit_local_notice(tx, state, store, "Autonomous Error", error),
                    }
                }
                "stop" => match state.set_autonomous_run_state(AutonomousRunPatch {
                    is_autonomous_running: Some(false),
                    autonomous_pause_reason: Some(Some("Interrupted by user.".to_string())),
                    autonomous_stop_reason: Some(None),
                    ..AutonomousRunPatch::default()
                }) {
                    Ok(write) => {
                        persist_write(tx.clone(), store.clone(), write);
                        emit_local_notice(
                            tx,
                            state,
                            store,
                            "Autonomous",
                            "Autonomous proof loop paused.".to_string(),
                        );
                    }
                    Err(error) => emit_local_notice(tx, state, store, "Autonomous Error", error),
                },
                "step" => {
                    match run_autonomous_step(tx.clone(), store.clone(), state) {
                        Ok(message) => emit_local_notice(tx, state, store, "Autonomous", message),
                        Err(error) => emit_local_notice(tx, state, store, "Autonomous Error", error),
                    }
                }
                _ => emit_local_notice(
                    tx,
                    state,
                    store,
                    "Autonomous Usage",
                    "Usage: /autonomous status|start|stop|step".to_string(),
                ),
            }
        }
        "/answer" => {
            let Some(question) = state
                .current_session()
                .and_then(|session| session.proof.pending_question.clone())
            else {
                emit_local_notice(
                    tx,
                    state,
                    store,
                    "Answer Error",
                    "No pending clarification question.".to_string(),
                );
                return;
            };
            if arg_text.is_empty() {
                emit_local_notice(
                    tx,
                    state,
                    store,
                    "Answer Usage",
                    "Usage: /answer <option-id|text>".to_string(),
                );
                return;
            }
            let reply_text = question
                .options
                .iter()
                .find(|option| option.id == arg_text)
                .map(|option| {
                    if option.formal_target.trim().is_empty() {
                        option.label.clone()
                    } else {
                        option.formal_target.clone()
                    }
                })
                .unwrap_or_else(|| arg_text.to_string());
            if let Some(submitted) = state.submit_text(reply_text) {
                persist_write(tx.clone(), store.clone(), openproof_core::PendingWrite {
                    session: submitted.session_snapshot.clone(),
                });
                handle_submission(tx, store, state, submitted);
            } else {
                emit_local_notice(
                    tx,
                    state,
                    store,
                    "Answer Error",
                    "Could not submit clarification answer.".to_string(),
                );
            }
        }
        "/theorem" => apply_statement_command(tx, state, store, ProofNodeKind::Theorem, arg_text),
        "/lemma" => apply_statement_command(tx, state, store, ProofNodeKind::Lemma, arg_text),
        "/verify" => start_verify_active_node(tx, state, store),
        "/login" => {
            emit_local_notice(
                tx,
                state,
                store,
                "Login",
                "Use `openproof login` from another shell to import the current Codex ChatGPT login.".to_string(),
            );
        }
        "/dashboard" => {
            let store_dash = store.clone();
            let tx_dash = tx.clone();
            let lean_dir = resolve_lean_project_dir();
            tokio::spawn(async move {
                match start_dashboard_server(store_dash, lean_dir, None).await {
                    Ok(server) => {
                        let url = format!("http://127.0.0.1:{}", server.port);
                        open_browser(&url);
                        let _ = tx_dash.send(AppEvent::AppendNotice {
                            title: "Dashboard".to_string(),
                            content: format!("Dashboard opened at {url}"),
                        });
                    }
                    Err(e) => {
                        let _ = tx_dash.send(AppEvent::AppendNotice {
                            title: "Dashboard Error".to_string(),
                            content: format!("Could not start dashboard: {e}"),
                        });
                    }
                }
            });
        }
        "/sessions" => {
            // Open interactive session picker.
            state.overlay = Some(openproof_core::Overlay::SessionPicker {
                selected: state.selected_session,
            });
        }
        _ => {
            emit_local_notice(
                tx,
                state,
                store,
                "Unknown Command",
                format!("Unknown local command: {trimmed}"),
            );
        }
    }
}

async fn build_turn_messages_with_retrieval(
    store: &AppStore,
    session: Option<&SessionSnapshot>,
) -> Vec<TurnMessage> {
    let retrieval = retrieval_context(store, session).await;
    let mut system_prompt = build_system_prompt(session);
    if !retrieval.trim().is_empty() {
        system_prompt.push_str("\n\n");
        system_prompt.push_str(&retrieval);
    }
    let mut messages = vec![TurnMessage {
        role: "system".to_string(),
        content: system_prompt,
    }];
    if let Some(session) = session {
        let recent = session
            .transcript
            .iter()
            .cloned()
            .rev()
            .take(12)
            .collect::<Vec<_>>();
        for entry in recent.into_iter().rev() {
            if let Some(message) = transcript_entry_to_turn_message(entry) {
                messages.push(message);
            }
        }
    }
    messages
}

async fn build_branch_turn_messages(
    store: &AppStore,
    session: &SessionSnapshot,
    role: AgentRole,
    title: &str,
    branch_id: &str,
) -> Vec<TurnMessage> {
    let retrieval = retrieval_context(store, Some(session)).await;
    let mut messages = vec![TurnMessage {
        role: "system".to_string(),
        content: [
            build_system_prompt(Some(session)),
            retrieval,
            format!("You are the {} branch for OpenProof.", agent_role_label(role)),
            format!("Branch id: {branch_id}"),
            format!("Task: {title}"),
            "Respond with concrete progress only. Use structured markers like PHASE, STATUS, NEXT, PAPER, THEOREM, LEMMA, and fenced ```lean``` blocks when useful."
                .to_string(),
            match role {
                AgentRole::Planner => {
                    "Focus on strategy refinement, decomposition, and lemma planning.".to_string()
                }
                AgentRole::Retriever => {
                    "Focus on retrieving exact declaration names, likely lemmas, and imports.".to_string()
                }
                AgentRole::Prover => {
                    "Focus on producing a compilable Lean candidate for the active target.".to_string()
                }
                AgentRole::Repairer => {
                    "Focus on repairing the current Lean candidate using the latest diagnostics.".to_string()
                }
                AgentRole::Critic => {
                    "Focus on finding gaps, hidden assumptions, and likely failure modes.".to_string()
                }
            },
        ]
        .join("\n\n"),
    }];
    messages.push(TurnMessage {
        role: "user".to_string(),
        content: format!(
            "Continue the branch task now.\nRole: {}\nTask: {}",
            agent_role_label(role),
            title
        ),
    });
    messages
}

async fn retrieval_context(store: &AppStore, session: Option<&SessionSnapshot>) -> String {
    let Some(session) = session else {
        return String::new();
    };
    let query = session
        .proof
        .active_node_id
        .as_deref()
        .and_then(|id| session.proof.nodes.iter().find(|node| node.id == id))
        .map(|node| node.statement.clone())
        .or_else(|| session.proof.accepted_target.clone())
        .or_else(|| session.proof.formal_target.clone())
        .or_else(|| {
            session
                .transcript
                .iter()
                .rev()
                .find(|entry| entry.role == MessageRole::User)
                .map(|entry| entry.content.clone())
        })
        .unwrap_or_default();
    let query = query.trim().to_string();
    if query.is_empty() {
        return String::new();
    }

    let mut sections = Vec::new();
    if let Ok(local_hits) = store.search_verified_corpus(&query, 4) {
        if !local_hits.is_empty() {
            sections.push(format!(
                "Local verified corpus hits:\n{}",
                local_hits
                    .into_iter()
                    .map(|(label, statement, visibility)| {
                        format!("- {} [{}] :: {}", label, visibility, statement)
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }
    }
    if let Some(remote_hits) = remote_verified_hits(session, &query, 4).await {
        if !remote_hits.is_empty() {
            sections.push(format!(
                "Remote verified corpus hits:\n{}",
                remote_hits.join("\n")
            ));
        }
    }
    sections.join("\n\n")
}

async fn remote_verified_hits(
    session: &SessionSnapshot,
    query: &str,
    limit: usize,
) -> Option<Vec<String>> {
    if session.cloud.share_mode == ShareMode::Local {
        return None;
    }
    let client = openproof_cloud::CloudCorpusClient::new(Default::default());
    let hits = client
        .search_verified_remote(query, limit, session.cloud.share_mode, None)
        .await
        .ok()?;
    Some(
        hits.into_iter()
            .take(limit)
            .map(|hit| format!("- {} [{}] :: {}", hit.label, hit.visibility, hit.statement))
            .collect(),
    )
}

fn summarize_branch_output(content: &str) -> String {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("NEXT:") {
            let value = value.trim();
            if !value.is_empty() {
                return value.to_string();
            }
        }
        if let Some(value) = trimmed.strip_prefix("STATUS:") {
            let value = value.trim();
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }
    truncate(content, 160)
}

fn branch_phase_for_role(role: AgentRole) -> &'static str {
    match role {
        AgentRole::Planner => "planning",
        AgentRole::Retriever => "retrieving",
        AgentRole::Prover => "proving",
        AgentRole::Repairer => "repairing",
        AgentRole::Critic => "blocked",
    }
}

fn truncate(input: &str, limit: usize) -> String {
    let trimmed = input.trim();
    if trimmed.chars().count() <= limit {
        return trimmed.to_string();
    }
    trimmed
        .chars()
        .take(limit.saturating_sub(1))
        .collect::<String>()
        + "…"
}

fn transcript_entry_to_turn_message(entry: TranscriptEntry) -> Option<TurnMessage> {
    let role = match entry.role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::System => "system",
        MessageRole::Notice => return None,
    };
    Some(TurnMessage {
        role: role.to_string(),
        content: entry.content,
    })
}

fn apply_statement_command(
    tx: mpsc::UnboundedSender<AppEvent>,
    state: &mut AppState,
    store: AppStore,
    kind: ProofNodeKind,
    arg_text: &str,
) {
    let Some((label, statement)) = parse_statement_args(arg_text) else {
        let usage = match kind {
            ProofNodeKind::Theorem => "Usage: /theorem <label> :: <statement>",
            ProofNodeKind::Lemma => "Usage: /lemma <label> :: <statement>",
            _ => "Usage: /<kind> <label> :: <statement>",
        };
        emit_local_notice(tx, state, store, "Usage", usage.to_string());
        return;
    };
    match state.add_proof_node(kind, &label, &statement) {
        Ok(write) => persist_write(tx, store, write),
        Err(error) => emit_local_notice(tx, state, store, "Statement Error", error),
    }
}

fn start_verify_active_node(
    tx: mpsc::UnboundedSender<AppEvent>,
    state: &mut AppState,
    store: AppStore,
) {
    if state.verification_in_flight {
        emit_local_notice(
            tx,
            state,
            store,
            "Verify Busy",
            "Lean verification is already running.".to_string(),
        );
        return;
    }

    let session = match state.current_session().cloned() {
        Some(session) => session,
        None => {
            emit_local_notice(tx, state, store, "Verify Error", "No active session.".to_string());
            return;
        }
    };

    let mut verification_session = session.clone();
    if let Some(active_branch_id) = session.proof.active_branch_id.as_deref() {
        if let Some(branch) = session
            .proof
            .branches
            .iter()
            .find(|branch| branch.id == active_branch_id)
        {
            if !branch.lean_snippet.trim().is_empty() {
                if let Some(focus_node_id) = branch
                    .focus_node_id
                    .as_deref()
                    .or(session.proof.active_node_id.as_deref())
                {
                    verification_session.proof.active_node_id = Some(focus_node_id.to_string());
                    if let Some(node) = verification_session
                        .proof
                        .nodes
                        .iter_mut()
                        .find(|node| node.id == focus_node_id)
                    {
                        node.content = branch.lean_snippet.clone();
                    }
                }
            }
        }
    }

    if verification_session.proof.active_node_id.is_none() {
        emit_local_notice(
            tx,
            state,
            store,
            "Verify Error",
            "No active proof node is focused.".to_string(),
        );
        return;
    }

    if let Some(write) = state.apply(AppEvent::LeanVerifyStarted) {
        persist_write(tx.clone(), store.clone(), write);
    }
    let project_dir = env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("lean");
    let tx_verify = tx.clone();
    tokio::spawn(async move {
        let outcome = tokio::task::spawn_blocking(move || verify_active_node(&project_dir, &verification_session))
            .await
            .ok()
            .and_then(Result::ok);
        match outcome {
            Some(result) => {
                let _ = tx_verify.send(AppEvent::LeanVerifyFinished(result));
            }
            None => {
                let _ = tx_verify.send(AppEvent::AppendNotice {
                    title: "Verify Error".to_string(),
                    content: "Lean verification crashed.".to_string(),
                });
            }
        }
    });
}

fn emit_local_notice(
    tx: mpsc::UnboundedSender<AppEvent>,
    state: &mut AppState,
    store: AppStore,
    title: &str,
    content: String,
) {
    if let Some(write) = state.apply(AppEvent::AppendNotice {
        title: title.to_string(),
        content,
    }) {
        persist_write(tx, store, write);
    }
}

fn parse_statement_args(arg_text: &str) -> Option<(String, String)> {
    let (label, statement) = arg_text.split_once("::")?;
    let label = label.trim();
    let statement = statement.trim();
    if label.is_empty() || statement.is_empty() {
        None
    } else {
        Some((label.to_string(), statement.to_string()))
    }
}

fn build_system_prompt(session: Option<&SessionSnapshot>) -> String {
    let prompt_context = load_prompt_context();
    let mut sections = vec![
        "You are openproof, a concise formal math assistant working in a persistent terminal session.".to_string(),
        "Keep momentum, be direct, and when a proof node is active prefer concrete Lean progress over exposition.".to_string(),
        "When writing Lean 4 proofs: prefer well-known tactics (simp, omega, ring, norm_num, exact?, apply?, rw?) over guessing exact lemma names. If unsure of an exact Mathlib lemma name, use `exact?` or `apply?` to let Lean search at compile time. This avoids hallucinated lemma names that cause Unknown constant errors. Use fully-qualified names like `RingHom.ker f` instead of dot notation `f.ker` when field notation may not be available. Prefer `n.factorial` over `n!` notation. Use `Nat.Prime p` as the type for prime hypotheses.".to_string(),
        "When formalizing or continuing a proof, prefer structured progress markers such as TITLE, PROBLEM, FORMAL_TARGET, ACCEPTED_TARGET, PHASE, STATUS, QUESTION, OPTION, OPTION_TARGET, RECOMMENDED_OPTION, THEOREM, LEMMA, PAPER, NEXT, and fenced ```lean``` blocks when relevant.".to_string(),
    ];
    if !prompt_context.instructions.trim().is_empty() {
        sections.push(format!("Loaded instructions:\n{}", prompt_context.instructions.trim()));
    }
    if !prompt_context.memory.trim().is_empty() {
        sections.push(format!("Remembered context:\n{}", prompt_context.memory.trim()));
    }
    if let Some(session) = session {
        if let Some(problem) = session.proof.problem.as_ref().filter(|item| !item.trim().is_empty()) {
            sections.push(format!("Problem: {}", problem.trim()));
        }
        if let Some(formal_target) = session
            .proof
            .accepted_target
            .as_ref()
            .or(session.proof.formal_target.as_ref())
            .filter(|item| !item.trim().is_empty())
        {
            sections.push(format!("Formal target: {}", formal_target.trim()));
            if session
                .proof
                .accepted_target
                .as_ref()
                .filter(|item| !item.trim().is_empty())
                .is_some()
            {
                sections.push(
                    "The target is accepted. Continue autonomously toward a Lean-verifiable proof candidate instead of re-asking for clarification."
                        .to_string(),
                );
            } else {
                sections.push(
                    "The target is not fully accepted yet. If it becomes clear, emit ACCEPTED_TARGET and continue."
                        .to_string(),
                );
            }
        }
        if !session.proof.assumptions.is_empty() {
            sections.push(format!(
                "Assumptions:\n{}",
                session
                    .proof
                    .assumptions
                    .iter()
                    .map(|item| format!("- {}", item))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }
        if let Some(question) = session.proof.pending_question.as_ref() {
            let mut question_block = vec![format!("Open clarification question: {}", question.prompt)];
            if !question.options.is_empty() {
                question_block.extend(question.options.iter().map(|option| {
                    let recommended = question
                        .recommended_option_id
                        .as_ref()
                        .map(|value| value == &option.id)
                        .unwrap_or(false);
                    format!(
                        "- {}{}: {}{}",
                        option.id,
                        if recommended { " [recommended]" } else { "" },
                        option.label,
                        if option.formal_target.trim().is_empty() {
                            String::new()
                        } else {
                            format!(" :: {}", option.formal_target.trim())
                        }
                    )
                }));
            }
            if let Some(answer) = question.answer_text.as_ref().filter(|item| !item.trim().is_empty()) {
                question_block.push(format!("Latest user answer: {}", answer.trim()));
                question_block.push(
                    "Use the user's answer to resolve the clarification if possible. Emit ACCEPTED_TARGET once the target is clear."
                        .to_string(),
                );
            } else {
                question_block.push(
                    "If clarification is still required, emit QUESTION / OPTION / OPTION_TARGET / RECOMMENDED_OPTION lines."
                        .to_string(),
                );
            }
            sections.push(question_block.join("\n"));
        }
        if let Some(active_node_id) = session.proof.active_node_id.as_deref() {
            if let Some(node) = session.proof.nodes.iter().find(|node| node.id == active_node_id) {
                sections.push(format!(
                    "Active target: {} :: {}",
                    node.label, node.statement
                ));
                sections.push(
                    "If you have a concrete Lean candidate, include a fenced ```lean``` block for the active target."
                        .to_string(),
                );
                if !node.content.trim().is_empty() {
                    sections.push(format!(
                        "Current candidate:\n```lean\n{}\n```",
                        node.content.trim()
                    ));
                }
            }
        }
    }
    sections.join("\n\n")
}

fn load_prompt_context() -> PromptContextFiles {
    let base_dirs = BaseDirs::new();
    let home = base_dirs
        .as_ref()
        .map(|dirs| dirs.home_dir().join(".openproof"))
        .unwrap_or_else(|| PathBuf::from(".openproof"));
    let launch_cwd = env::var("OPENPROOF_LAUNCH_CWD")
        .map(PathBuf::from)
        .unwrap_or_else(|_| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let agents_paths = vec![home.join("AGENTS.md"), launch_cwd.join("AGENTS.md")]
        .into_iter()
        .filter(|path| path.exists())
        .fold(Vec::<PathBuf>::new(), |mut acc, path| {
            if !acc.contains(&path) {
                acc.push(path);
            }
            acc
        });
    let instructions = agents_paths
        .iter()
        .filter_map(|path| read_text_if_exists(path).map(|content| (path, content)))
        .filter(|(_, content)| !content.trim().is_empty())
        .map(|(path, content)| format!("# {}\n{}", path.display(), content.trim()))
        .collect::<Vec<_>>()
        .join("\n\n");

    let global_memory_path = home.join("memory").join("global.md");
    let workspace_memory_path = home
        .join("memory")
        .join("workspaces")
        .join(format!("{}.md", sanitize_workspace_path(&launch_cwd)));
    let global_content = ensure_and_read_text(&global_memory_path).unwrap_or_default();
    let workspace_content = ensure_and_read_text(&workspace_memory_path).unwrap_or_default();
    let memory = [
        if global_content.trim().is_empty() {
            None
        } else {
            Some(format!("# Global memory\n{}", global_content.trim()))
        },
        if workspace_content.trim().is_empty() {
            None
        } else {
            Some(format!("# Workspace memory\n{}", workspace_content.trim()))
        },
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n\n");

    PromptContextFiles {
        instructions,
        global_memory_path,
        workspace_memory_path,
        memory,
    }
}

fn read_text_if_exists(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

fn ensure_and_read_text(path: &Path) -> Result<String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if !path.exists() {
        fs::write(path, "")?;
    }
    Ok(fs::read_to_string(path).unwrap_or_default())
}

fn append_memory_entry(path: &Path, text: &str) -> Result<String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let line = format!("- {} {}\n", chrono::Utc::now().to_rfc3339(), text.trim());
    let mut content = if path.exists() {
        fs::read_to_string(path).unwrap_or_default()
    } else {
        String::new()
    };
    content.push_str(&line);
    fs::write(path, content)?;
    Ok(line.trim().to_string())
}

fn export_session_artifacts(session: Option<&SessionSnapshot>, target: &str) -> Result<Vec<String>> {
    let Some(session) = session else {
        bail!("No active session.");
    };
    let base_dirs = BaseDirs::new().ok_or_else(|| anyhow::anyhow!("could not resolve home directory"))?;
    let export_dir = base_dirs
        .home_dir()
        .join(".openproof")
        .join("exports")
        .join(&session.id);
    fs::create_dir_all(&export_dir)?;

    let mut written = Vec::new();
    if matches!(target, "paper" | "all") {
        let path = export_dir.join("paper.txt");
        fs::write(&path, render_paper_text(session))?;
        written.push(path.display().to_string());
    }
    if matches!(target, "tex" | "all") {
        let path = export_dir.join("paper.tex");
        fs::write(&path, render_paper_tex(session))?;
        written.push(path.display().to_string());
    }
    if matches!(target, "lean" | "all") {
        if let Some(node) = session
            .proof
            .active_node_id
            .as_deref()
            .and_then(|id| session.proof.nodes.iter().find(|node| node.id == id))
        {
            if !node.content.trim().is_empty() {
                let path = export_dir.join(format!("{}.lean", sanitize_file_stem(&node.label)));
                fs::write(&path, node.content.trim())?;
                written.push(path.display().to_string());
            }
        }
        if let Some(branch) = session
            .proof
            .active_branch_id
            .as_deref()
            .and_then(|id| session.proof.branches.iter().find(|branch| branch.id == id))
        {
            if !branch.lean_snippet.trim().is_empty() {
                let path = export_dir.join(format!("{}_branch.lean", sanitize_file_stem(&branch.title)));
                fs::write(&path, branch.lean_snippet.trim())?;
                written.push(path.display().to_string());
            }
        }
    }
    if target == "all" {
        let path = export_dir.join("session.json");
        fs::write(&path, format!("{}\n", serde_json::to_string_pretty(session)?))?;
        written.push(path.display().to_string());
    }
    if !matches!(target, "paper" | "tex" | "lean" | "all") {
        bail!("Usage: /export paper|tex|lean|all");
    }
    Ok(written)
}

fn run_autonomous_step(
    tx: mpsc::UnboundedSender<AppEvent>,
    store: AppStore,
    state: &mut AppState,
) -> Result<String, String> {
    let session = state
        .current_session()
        .cloned()
        .ok_or_else(|| "No active session.".to_string())?;
    if let Some(reason) = autonomous_stop_reason(&session)
        .filter(|reason| !reason.contains("completed the current proof run"))
    {
        return Err(reason);
    }

    let target = session
        .proof
        .accepted_target
        .clone()
        .or(session.proof.formal_target.clone())
        .ok_or_else(|| "Set or accept a formal target before running autonomous search.".to_string())?;

    let next_iteration = session.proof.autonomous_iteration_count.saturating_add(1);
    if let Ok(write) = state.set_autonomous_run_state(AutonomousRunPatch {
        autonomous_iteration_count: Some(next_iteration),
        autonomous_pause_reason: Some(None),
        autonomous_stop_reason: Some(None),
        ..AutonomousRunPatch::default()
    }) {
        persist_write(tx.clone(), store.clone(), write);
    }

    let mut actions = Vec::new();
    if let Ok(summary) = refresh_retrieval_branch(tx.clone(), store.clone(), state) {
        actions.push(summary);
    }

    let latest_session = state
        .current_session()
        .cloned()
        .ok_or_else(|| "No active session.".to_string())?;

    let best_hidden = best_hidden_branch(&latest_session).cloned();
    let current_foreground = current_foreground_branch(Some(&latest_session)).cloned();
    if should_promote_hidden_branch(best_hidden.clone(), current_foreground.clone()) {
        if let Some(candidate) = best_hidden {
            let reason = format!("Promoted stronger hidden branch {}.", candidate.title);
            if let Ok(write) = state.promote_branch_to_foreground(&candidate.id, false, Some(&reason))
            {
                persist_write(tx.clone(), store.clone(), write);
                actions.push(reason);
            }
        }
    }

    let latest_session = state
        .current_session()
        .cloned()
        .ok_or_else(|| "No active session.".to_string())?;
    let repair_basis = current_foreground_branch(Some(&latest_session))
        .filter(|branch| {
            branch
                .latest_diagnostics
                .as_ref()
                .map(|item| !item.trim().is_empty())
                .unwrap_or(false)
                || !branch.last_lean_diagnostic.trim().is_empty()
        })
        .cloned()
        .or_else(|| {
            best_hidden_branch(&latest_session)
                .filter(|branch| {
                    branch
                        .latest_diagnostics
                        .as_ref()
                        .map(|item| !item.trim().is_empty())
                        .unwrap_or(false)
                        || !branch.last_lean_diagnostic.trim().is_empty()
                })
                .cloned()
        });

    if let Some(basis) = repair_basis {
        let description = format!(
            "Repair the failing Lean candidate for {} using the latest diagnostics.",
            target
        );
        let title = format!("{} repair", latest_session.title);
        let (branch_id, session_snapshot) = ensure_hidden_agent_branch(
            tx.clone(),
            store.clone(),
            state,
            AgentRole::Repairer,
            &title,
            &description,
        )?;
        start_agent_branch_turn(
            tx,
            store,
            AgentRole::Repairer,
            format!("{description}\n\nLatest diagnostics:\n{}", basis.last_lean_diagnostic),
            branch_id.clone(),
            branch_id.clone(),
            session_snapshot,
        );
        actions.push(format!("Started repairer branch {branch_id}."));
        return Ok(actions.join("\n"));
    }

    if latest_session
        .proof
        .strategy_summary
        .as_ref()
        .map(|item| item.trim().is_empty())
        .unwrap_or(true)
    {
        let description = format!("Refine a proof plan for {target}.");
        let title = format!("{} planner", latest_session.title);
        let (branch_id, session_snapshot) = ensure_hidden_agent_branch(
            tx.clone(),
            store.clone(),
            state,
            AgentRole::Planner,
            &title,
            &description,
        )?;
        start_agent_branch_turn(
            tx.clone(),
            store.clone(),
            AgentRole::Planner,
            description,
            branch_id.clone(),
            branch_id.clone(),
            session_snapshot,
        );
        actions.push(format!("Started planner branch {branch_id}."));
    }

    let latest_session = state
        .current_session()
        .cloned()
        .ok_or_else(|| "No active session.".to_string())?;
    let has_foreground = current_foreground_branch(Some(&latest_session)).is_some();
    if has_foreground {
        let description = format!("Produce an alternate Lean proof candidate for {target}.");
        let title = format!("{} search prover", latest_session.title);
        let (branch_id, session_snapshot) = ensure_hidden_agent_branch(
            tx.clone(),
            store.clone(),
            state,
            AgentRole::Prover,
            &title,
            &description,
        )?;
        start_agent_branch_turn(
            tx,
            store,
            AgentRole::Prover,
            description,
            branch_id.clone(),
            branch_id.clone(),
            session_snapshot,
        );
        actions.push(format!("Started hidden prover branch {branch_id}."));
    } else {
        let title = format!("{} prover", latest_session.title);
        let description = format!("Produce a Lean proof candidate for {target}.");
        let (write, branch_id, task_id) =
            state.spawn_agent_branch(AgentRole::Prover, &title, &description, false)?;
        let session_snapshot = write.session.clone();
        persist_write(tx.clone(), store.clone(), write);
        start_agent_branch_turn(
            tx,
            store,
            AgentRole::Prover,
            description,
            branch_id.clone(),
            task_id,
            session_snapshot,
        );
        actions.push(format!("Started foreground prover branch {branch_id}."));
    }

    if actions.is_empty() {
        Ok("Autonomous loop found no new branch to schedule.".to_string())
    } else {
        Ok(actions.join("\n"))
    }
}

fn render_paper_text(session: &SessionSnapshot) -> String {
    let mut lines = vec![
        format!("Title: {}", session.title),
        format!(
            "Problem: {}",
            session
                .proof
                .problem
                .clone()
                .unwrap_or_else(|| "none".to_string())
        ),
        format!(
            "Formal target: {}",
            session
                .proof
                .formal_target
                .clone()
                .unwrap_or_else(|| "none".to_string())
        ),
        format!(
            "Accepted target: {}",
            session
                .proof
                .accepted_target
                .clone()
                .unwrap_or_else(|| "none".to_string())
        ),
        String::new(),
        "Notes:".to_string(),
    ];
    if session.proof.paper_notes.is_empty() {
        lines.push("No paper notes yet.".to_string());
    } else {
        lines.extend(
            session
                .proof
                .paper_notes
                .iter()
                .enumerate()
                .map(|(index, note)| format!("{}. {}", index + 1, note)),
        );
    }
    lines.join("\n")
}

fn render_paper_tex(session: &SessionSnapshot) -> String {
    format!(
        "\\section*{{{}}}\n\\textbf{{Problem:}} {}\\\\\n\\textbf{{Formal target:}} {}\\\\\n\\textbf{{Accepted target:}} {}\n\n\\begin{{itemize}}\n{}\n\\end{{itemize}}\n",
        escape_tex(&session.title),
        escape_tex(session.proof.problem.as_deref().unwrap_or("none")),
        escape_tex(session.proof.formal_target.as_deref().unwrap_or("none")),
        escape_tex(session.proof.accepted_target.as_deref().unwrap_or("none")),
        if session.proof.paper_notes.is_empty() {
            "\\item No paper notes yet.".to_string()
        } else {
            session
                .proof
                .paper_notes
                .iter()
                .map(|note| format!("\\item {}", escape_tex(note)))
                .collect::<Vec<_>>()
                .join("\n")
        }
    )
}

fn sanitize_file_stem(input: &str) -> String {
    let mut value = input
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect::<String>();
    while value.contains("__") {
        value = value.replace("__", "_");
    }
    let value = value.trim_matches('_').to_string();
    if value.is_empty() {
        "artifact".to_string()
    } else {
        value
    }
}

fn escape_tex(input: &str) -> String {
    input
        .replace('\\', "\\textbackslash{}")
        .replace('{', "\\{")
        .replace('}', "\\}")
        .replace('_', "\\_")
        .replace('&', "\\&")
        .replace('%', "\\%")
        .replace('$', "\\$")
        .replace('#', "\\#")
}

fn parse_share_mode(value: &str) -> Option<ShareMode> {
    match value.trim() {
        "local" => Some(ShareMode::Local),
        "community" => Some(ShareMode::Community),
        "private" => Some(ShareMode::Private),
        _ => None,
    }
}

fn parse_agent_role(value: &str) -> Option<AgentRole> {
    match value.trim() {
        "planner" => Some(AgentRole::Planner),
        "prover" => Some(AgentRole::Prover),
        "repairer" => Some(AgentRole::Repairer),
        "retriever" => Some(AgentRole::Retriever),
        "critic" => Some(AgentRole::Critic),
        _ => None,
    }
}

fn agent_role_label(role: AgentRole) -> &'static str {
    match role {
        AgentRole::Planner => "planner",
        AgentRole::Prover => "prover",
        AgentRole::Repairer => "repairer",
        AgentRole::Retriever => "retriever",
        AgentRole::Critic => "critic",
    }
}

fn share_mode_label(mode: ShareMode) -> &'static str {
    match mode {
        ShareMode::Local => "local",
        ShareMode::Community => "community",
        ShareMode::Private => "private",
    }
}

fn describe_remote_corpus() -> String {
    let client = openproof_cloud::CloudCorpusClient::new(Default::default());
    client.describe()
}

fn start_sync_drain(
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
        let corpus = openproof_corpus::CorpusManager::new(
            store,
            cloud_client,
            PathBuf::from("."),
        );
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

fn sanitize_workspace_path(path: &Path) -> String {
    let mut value = path
        .to_string_lossy()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect::<String>();
    while value.contains("__") {
        value = value.replace("__", "_");
    }
    value = value.trim_matches('_').to_string();
    if value.is_empty() {
        "workspace".to_string()
    } else {
        value
    }
}
