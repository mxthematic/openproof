use anyhow::Result;
use axum::{
    extract::{Query, State},
    http::{header::CONTENT_TYPE, StatusCode},
    response::{Html, IntoResponse},
    routing::get,
    Json, Router,
};
use openproof_lean::detect_lean_health;
use openproof_model::load_auth_summary;
use openproof_protocol::{
    DashboardSessionSummary, DashboardStatusResponse, HealthReport, MessageRole, SessionSnapshot,
};
use openproof_store::AppStore;
use std::{net::SocketAddr, process::Command, sync::Arc};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};

const INDEX_HTML: &str = include_str!("../static/index.html");
const APP_JS: &str = include_str!("../static/app.js");
const STYLES_CSS: &str = include_str!("../static/styles.css");

#[derive(Clone)]
struct DashboardState {
    store: AppStore,
    lean_project_dir: std::path::PathBuf,
}

pub struct DashboardServer {
    pub port: u16,
    shutdown_tx: Option<oneshot::Sender<()>>,
    handle: JoinHandle<()>,
}

impl DashboardServer {
    pub async fn close(mut self) -> Result<()> {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        let _ = self.handle.await;
        Ok(())
    }
}

pub async fn start_dashboard_server(
    store: AppStore,
    lean_project_dir: std::path::PathBuf,
    preferred_port: Option<u16>,
) -> Result<DashboardServer> {
    let state = Arc::new(DashboardState {
        store,
        lean_project_dir,
    });

    let router = Router::new()
        .route("/", get(index))
        .route("/app.js", get(app_js))
        .route("/styles.css", get(styles_css))
        .route("/api/status", get(status))
        .route("/api/health", get(health))
        .route("/api/sessions", get(sessions))
        .route("/api/session", get(session))
        .route("/api/raw-state", get(status))
        .route("/api/paper/tex", get(paper_tex))
        .route("/api/paper/pdf", get(paper_pdf))
        .route("/api/workspace", get(workspace_files))
        .with_state(state);

    let primary_port = preferred_port.unwrap_or(4821);
    let listener = match TcpListener::bind(("127.0.0.1", primary_port)).await {
        Ok(listener) => listener,
        Err(_) => TcpListener::bind(("127.0.0.1", 0)).await?,
    };
    let port = listener.local_addr()?.port();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let handle = tokio::spawn(async move {
        let server = axum::serve(listener, router).with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        });
        let _ = server.await;
    });

    Ok(DashboardServer {
        port,
        shutdown_tx: Some(shutdown_tx),
        handle,
    })
}

pub fn open_browser(url: &str) {
    let platform = std::env::consts::OS;
    let mut command = if platform == "macos" {
        let mut cmd = Command::new("open");
        cmd.arg(url);
        cmd
    } else if platform == "windows" {
        let mut cmd = Command::new("cmd");
        cmd.args(["/c", "start", "", url]);
        cmd
    } else {
        let mut cmd = Command::new("xdg-open");
        cmd.arg(url);
        cmd
    };
    let _ = command.spawn();
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn app_js() -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "application/javascript; charset=utf-8")],
        APP_JS,
    )
}

async fn styles_css() -> impl IntoResponse {
    ([(CONTENT_TYPE, "text/css; charset=utf-8")], STYLES_CSS)
}

async fn status(
    State(state): State<Arc<DashboardState>>,
) -> Result<Json<DashboardStatusResponse>, (StatusCode, String)> {
    let sessions = state.store.list_sessions().map_err(internal_error)?;
    let auth = load_auth_summary().unwrap_or_default();
    let lean = detect_lean_health(&state.lean_project_dir).unwrap_or_default();
    let payload = DashboardStatusResponse {
        local_db_path: state.store.db_path().display().to_string(),
        auth,
        lean,
        session_count: sessions.len(),
        active_session_id: sessions.first().map(|session| session.id.clone()),
        sessions: sessions.iter().map(build_session_summary).collect(),
    };
    Ok(Json(payload))
}

async fn health(
    State(state): State<Arc<DashboardState>>,
) -> Result<Json<HealthReport>, (StatusCode, String)> {
    let latest_session = state.store.latest_session().map_err(internal_error)?;
    let auth = load_auth_summary().unwrap_or_default();
    let lean = detect_lean_health(&state.lean_project_dir).unwrap_or_default();
    let payload = HealthReport {
        ok: lean.ok,
        local_db_path: state.store.db_path().display().to_string(),
        session_count: state.store.session_count().map_err(internal_error)?,
        latest_session_id: latest_session.map(|session| session.id),
        auth,
        lean,
    };
    Ok(Json(payload))
}

async fn sessions(
    State(state): State<Arc<DashboardState>>,
) -> Result<Json<Vec<DashboardSessionSummary>>, (StatusCode, String)> {
    let sessions = state
        .store
        .list_sessions()
        .map_err(internal_error)?
        .iter()
        .map(build_session_summary)
        .collect::<Vec<_>>();
    Ok(Json(sessions))
}

#[derive(Debug, serde::Deserialize)]
struct SessionQuery {
    id: Option<String>,
}

async fn session(
    State(state): State<Arc<DashboardState>>,
    Query(query): Query<SessionQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let session = match query.id.as_deref() {
        Some(id) => state.store.get_session(id).map_err(internal_error)?,
        None => state.store.latest_session().map_err(internal_error)?,
    };
    match session {
        Some(session) => Ok(Json(session).into_response()),
        None => Ok(StatusCode::NOT_FOUND.into_response()),
    }
}

async fn paper_tex(
    State(state): State<Arc<DashboardState>>,
    Query(query): Query<SessionQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let session = match query.id.as_deref() {
        Some(id) => state.store.get_session(id).map_err(internal_error)?,
        None => state.store.latest_session().map_err(internal_error)?,
    };
    let Some(session) = session else {
        return Ok((StatusCode::NOT_FOUND, "No session").into_response());
    };
    let tex = generate_tex(&session);
    Ok(([(CONTENT_TYPE, "text/plain; charset=utf-8")], tex).into_response())
}

async fn paper_pdf(
    State(state): State<Arc<DashboardState>>,
    Query(query): Query<SessionQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let session = match query.id.as_deref() {
        Some(id) => state.store.get_session(id).map_err(internal_error)?,
        None => state.store.latest_session().map_err(internal_error)?,
    };
    let Some(session) = session else {
        return Ok((StatusCode::NOT_FOUND, "No session").into_response());
    };
    let tex = generate_tex(&session);

    // Compile in a temp directory.
    let tmp = std::env::temp_dir().join("openproof-paper");
    let _ = std::fs::create_dir_all(&tmp);
    let tex_path = tmp.join("paper.tex");
    std::fs::write(&tex_path, &tex).map_err(|e| internal_error(e.into()))?;

    let output = Command::new("lualatex")
        .args(["-interaction=nonstopmode", "-halt-on-error", "paper.tex"])
        .current_dir(&tmp)
        .output()
        .map_err(|e| internal_error(anyhow::anyhow!("lualatex failed to start: {e}")))?;

    let pdf_path = tmp.join("paper.pdf");
    if !pdf_path.exists() {
        let stderr = String::from_utf8_lossy(&output.stdout);
        return Ok((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("lualatex failed:\n{stderr}"),
        )
            .into_response());
    }

    let pdf_bytes = std::fs::read(&pdf_path).map_err(|e| internal_error(e.into()))?;
    Ok(([(CONTENT_TYPE, "application/pdf")], pdf_bytes).into_response())
}

async fn workspace_files(
    State(state): State<Arc<DashboardState>>,
    Query(query): Query<SessionQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let session = match query.id.as_deref() {
        Some(id) => state.store.get_session(id).map_err(internal_error)?,
        None => state.store.latest_session().map_err(internal_error)?,
    };
    let Some(session) = session else {
        return Ok("[]".to_string().into_response());
    };
    let ws_dir = state.store.workspace_dir(&session.id);
    let mut result = String::from("[");
    let mut first = true;
    if let Ok(entries) = state.store.list_workspace_files(&session.id) {
        for (path, _size) in entries {
            if path.ends_with(".lean") && !path.contains("history/") {
                let content = std::fs::read_to_string(ws_dir.join(&path)).unwrap_or_default();
                if !first { result.push(','); }
                first = false;
                // Manual JSON to avoid serde_json dependency
                let escaped = content.replace('\\', "\\\\").replace('"', "\\\"")
                    .replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t");
                result.push_str(&format!(
                    "{{\"path\":\"{path}\",\"content\":\"{escaped}\"}}"
                ));
            }
        }
    }
    result.push(']');
    Ok(([(CONTENT_TYPE, "application/json")], result).into_response())
}

fn generate_tex(session: &SessionSnapshot) -> String {
    let proof = &session.proof;
    let title = &session.title;

    // If the model has written a LaTeX paper body, use it directly.
    if !proof.paper_tex.trim().is_empty() {
        // If paper_tex is already a complete document, return it as-is
        if proof.paper_tex.contains("\\documentclass") {
            return proof.paper_tex.clone();
        }
        let mut doc = String::new();
        doc.push_str("\\documentclass[11pt]{article}\n");
        doc.push_str("\\usepackage[margin=1in]{geometry}\n");
        doc.push_str("\\usepackage{fontspec}\n");
        doc.push_str("\\usepackage{amsmath,amssymb,amsthm}\n");
        doc.push_str("\\usepackage{listings}\n");
        doc.push_str("\\usepackage{xcolor}\n");
        doc.push_str("\\lstset{basicstyle=\\ttfamily\\small,breaklines=true,frame=single,backgroundcolor=\\color{gray!10},literate=\n");
        doc.push_str("  {ℕ}{{\\ensuremath{\\mathbb{N}}}}1\n");
        doc.push_str("  {ℝ}{{\\ensuremath{\\mathbb{R}}}}1\n");
        doc.push_str("  {ℤ}{{\\ensuremath{\\mathbb{Z}}}}1\n");
        doc.push_str("  {→}{{\\ensuremath{\\to}}}1\n");
        doc.push_str("  {←}{{\\ensuremath{\\leftarrow}}}1\n");
        doc.push_str("  {∀}{{\\ensuremath{\\forall}}}1\n");
        doc.push_str("  {∃}{{\\ensuremath{\\exists}}}1\n");
        doc.push_str("  {∧}{{\\ensuremath{\\land}}}1\n");
        doc.push_str("  {∨}{{\\ensuremath{\\lor}}}1\n");
        doc.push_str("  {≤}{{\\ensuremath{\\leq}}}1\n");
        doc.push_str("  {≥}{{\\ensuremath{\\geq}}}1\n");
        doc.push_str("  {≠}{{\\ensuremath{\\neq}}}1\n");
        doc.push_str("  {∈}{{\\ensuremath{\\in}}}1\n");
        doc.push_str("  {⟨}{{\\ensuremath{\\langle}}}1\n");
        doc.push_str("  {⟩}{{\\ensuremath{\\rangle}}}1\n");
        doc.push_str("  {λ}{{\\ensuremath{\\lambda}}}1\n");
        doc.push_str("  {∑}{{\\ensuremath{\\sum}}}1\n");
        doc.push_str("  {∞}{{\\ensuremath{\\infty}}}1\n");
        doc.push_str("}\n");
        doc.push_str("\\newtheorem{theorem}{Theorem}\n");
        doc.push_str("\\newtheorem{lemma}[theorem]{Lemma}\n");
        doc.push_str("\\newtheorem{proposition}[theorem]{Proposition}\n");
        doc.push_str(&format!("\n\\title{{{}}}\n", tex_escape(title)));
        doc.push_str("\\author{OpenProof}\n");
        doc.push_str("\\date{\\today}\n\n");
        doc.push_str("\\begin{document}\n\\maketitle\n\n");
        // Strip [language=Lean] etc. -- listings doesn't know Lean.
        let sanitized = proof.paper_tex
            .replace("[language=Lean]", "")
            .replace("[language=lean]", "")
            .replace("[language=lean4]", "")
            .replace("[language=Lean4]", "");
        doc.push_str(&sanitized);
        doc.push_str("\n\n\\end{document}\n");
        return doc;
    }

    // Fallback: mechanical generation from proof state.
    let mut doc = String::new();
    doc.push_str("\\documentclass[11pt]{article}\n");
    doc.push_str("\\usepackage[margin=1in]{geometry}\n");
    doc.push_str("\\usepackage{amsmath,amssymb,amsthm}\n");
    doc.push_str("\\usepackage{listings}\n");
    doc.push_str("\\usepackage{xcolor}\n");
    doc.push_str("\\lstset{basicstyle=\\ttfamily\\small,breaklines=true,frame=single,backgroundcolor=\\color{gray!10}}\n");
    doc.push_str("\\newtheorem{theorem}{Theorem}\n");
    doc.push_str("\\newtheorem{lemma}[theorem]{Lemma}\n");
    doc.push_str("\\newtheorem{proposition}[theorem]{Proposition}\n");
    doc.push_str("\n");
    doc.push_str(&format!("\\title{{{}}}\n", tex_escape(title)));
    doc.push_str("\\author{OpenProof}\n");
    doc.push_str("\\date{\\today}\n");
    doc.push_str("\n\\begin{document}\n\\maketitle\n\n");

    // Problem statement
    if let Some(problem) = &proof.problem {
        if !problem.trim().is_empty() {
            doc.push_str("\\section*{Problem}\n");
            doc.push_str(&tex_escape(problem));
            doc.push_str("\n\n");
        }
    }

    // Formal target
    if let Some(target) = &proof.formal_target {
        if !target.trim().is_empty() {
            doc.push_str("\\section*{Formal Target}\n");
            doc.push_str("\\begin{lstlisting}[language={}]\n");
            doc.push_str(target);
            doc.push_str("\n\\end{lstlisting}\n\n");
        }
    }

    // Proof nodes
    if !proof.nodes.is_empty() {
        doc.push_str("\\section{Proof Structure}\n\n");
        for node in &proof.nodes {
            let env = match node.kind {
                openproof_protocol::ProofNodeKind::Theorem => "theorem",
                openproof_protocol::ProofNodeKind::Lemma => "lemma",
                _ => "proposition",
            };
            let status_marker = match node.status {
                openproof_protocol::ProofNodeStatus::Verified => " \\textnormal{[\\textcolor{green!70!black}{verified}]}",
                openproof_protocol::ProofNodeStatus::Failed => " \\textnormal{[\\textcolor{red}{failed}]}",
                openproof_protocol::ProofNodeStatus::Proving => " \\textnormal{[\\textcolor{orange}{proving}]}",
                _ => "",
            };
            doc.push_str(&format!(
                "\\begin{{{env}}}[{}]{status_marker}\n",
                tex_escape(&node.label)
            ));
            if !node.statement.is_empty() {
                doc.push_str(&tex_escape(&node.statement));
                doc.push_str("\n");
            }
            doc.push_str(&format!("\\end{{{env}}}\n\n"));

            if !node.content.trim().is_empty() {
                doc.push_str("\\begin{lstlisting}[language={}]\n");
                doc.push_str(&node.content);
                doc.push_str("\n\\end{lstlisting}\n\n");
            }
        }
    }

    // Paper notes
    if !proof.paper_notes.is_empty() {
        doc.push_str("\\section{Notes}\n\n");
        doc.push_str("\\begin{itemize}\n");
        for note in &proof.paper_notes {
            doc.push_str(&format!("\\item {}\n", tex_escape(note)));
        }
        doc.push_str("\\end{itemize}\n\n");
    }

    // Strategy summary
    if let Some(strategy) = &proof.strategy_summary {
        if !strategy.trim().is_empty() {
            doc.push_str("\\section{Strategy}\n\n");
            doc.push_str(&tex_escape(strategy));
            doc.push_str("\n\n");
        }
    }

    doc.push_str("\\end{document}\n");
    doc
}

fn tex_escape(s: &str) -> String {
    s.replace('\\', "\\textbackslash{}")
        .replace('{', "\\{")
        .replace('}', "\\}")
        .replace('&', "\\&")
        .replace('%', "\\%")
        .replace('$', "\\$")
        .replace('#', "\\#")
        .replace('_', "\\_")
        .replace('^', "\\^{}")
        .replace('~', "\\~{}")
}

fn build_session_summary(session: &SessionSnapshot) -> DashboardSessionSummary {
    let last_entry = session.transcript.last();
    let active_node_label = session
        .proof
        .active_node_id
        .as_deref()
        .and_then(|id| session.proof.nodes.iter().find(|node| node.id == id))
        .map(|node| node.label.clone());
    DashboardSessionSummary {
        id: session.id.clone(),
        title: session.title.clone(),
        updated_at: session.updated_at.clone(),
        workspace_label: session.workspace_label.clone(),
        transcript_entries: session.transcript.len(),
        proof_nodes: session.proof.nodes.len(),
        active_node_label,
        proof_phase: Some(session.proof.phase.clone()),
        last_role: last_entry.map(|entry| match entry.role {
            MessageRole::User => "user".to_string(),
            MessageRole::Assistant => "assistant".to_string(),
            MessageRole::System => "system".to_string(),
            MessageRole::Notice => "notice".to_string(),
            MessageRole::ToolCall => "tool_call".to_string(),
            MessageRole::ToolResult => "tool_result".to_string(),
        }),
        last_excerpt: last_entry.map(|entry| match entry.role {
            MessageRole::ToolCall => {
                let name = entry.title.as_deref().unwrap_or("tool");
                format!(">> {name}()")
            }
            MessageRole::ToolResult => {
                let name = entry.title.as_deref().unwrap_or("tool");
                truncate(&format!("<< {name}: {}", entry.content), 180)
            }
            _ => truncate(&entry.content, 180),
        }),
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

fn internal_error(error: anyhow::Error) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
}

pub fn dashboard_url(port: u16) -> String {
    SocketAddr::from(([127, 0, 0, 1], port)).to_string()
}
