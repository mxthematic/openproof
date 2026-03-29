//! Web dashboard for OpenProof session inspection and management.

mod editor_routes;
mod manage;
mod routes;
mod tex;
mod websocket;

use anyhow::Result;
use axum::{
    routing::{any, get, post},
    Router,
};
use openproof_store::AppStore;
use std::{net::SocketAddr, sync::Arc};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};

const INDEX_HTML: &str = include_str!("../static/index.html");
const APP_JS: &str = include_str!("../static/app.js");
const STYLES_CSS: &str = include_str!("../static/styles.css");
const SESSIONS_JS: &str = include_str!("../static/sessions.js");
const GRAPH_JS: &str = include_str!("../static/graph.js");

// Editor dist directory is resolved at runtime relative to the executable.
// Built by scripts/build-editor.sh -> static/editor-dist/

#[derive(Clone)]
pub(crate) struct DashboardState {
    pub store: AppStore,
    pub lean_project_dir: std::path::PathBuf,
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
        .route("/", get(routes::index))
        .route("/app.js", get(routes::app_js))
        .route("/styles.css", get(routes::styles_css))
        .route("/sessions.js", get(routes::sessions_js))
        .route("/graph.js", get(routes::graph_js))
        .route("/api/status", get(routes::status))
        .route("/api/health", get(routes::health))
        .route("/api/sessions", get(routes::sessions))
        .route("/api/session-summaries", get(routes::session_summaries))
        .route(
            "/api/session",
            get(routes::session)
                .delete(manage::delete_session)
                .patch(manage::rename_session),
        )
        .route(
            "/api/sessions/bulk-delete",
            post(manage::bulk_delete_sessions),
        )
        .route("/api/raw-state", get(routes::status))
        .route("/api/paper/tex", get(routes::paper_tex))
        .route("/api/paper/pdf", get(routes::paper_pdf))
        .route("/api/workspace", get(routes::workspace_files))
        // Lean IDE editor routes.
        .route("/editor", get(editor_routes::editor_page))
        .route("/editor/assets/{*path}", get(editor_routes::editor_asset))
        .route(
            "/editor/infoview/{*path}",
            get(editor_routes::editor_infoview_asset),
        )
        // Infoview iframe loads assets from /infoview/ (hardcoded in lean4monaco).
        .route(
            "/infoview/{*path}",
            get(editor_routes::editor_infoview_asset),
        )
        .route(
            "/api/editor/file",
            get(editor_routes::read_file).post(editor_routes::write_file),
        )
        .route("/lean-ws", any(websocket::lean_ws_handler))
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
        let mut cmd = std::process::Command::new("open");
        cmd.arg(url);
        cmd
    } else if platform == "windows" {
        let mut cmd = std::process::Command::new("cmd");
        cmd.args(["/c", "start", "", url]);
        cmd
    } else {
        let mut cmd = std::process::Command::new("xdg-open");
        cmd.arg(url);
        cmd
    };
    let _ = command.spawn();
}

pub fn dashboard_url(port: u16) -> String {
    SocketAddr::from(([127, 0, 0, 1], port)).to_string()
}
