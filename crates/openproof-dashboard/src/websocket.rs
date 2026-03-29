//! WebSocket-to-LSP proxy for the Lean 4 editor.
//!
//! Bridges browser WebSocket connections to a `lake serve` child process,
//! translating between raw JSON WebSocket messages and Content-Length framed
//! LSP JSON-RPC over stdio. Rewrites file URIs between browser virtual paths
//! and the server's actual lean project directory.

use axum::{
    extract::{State, WebSocketUpgrade},
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use crate::DashboardState;

const VIRTUAL_PREFIX: &str = "file:///LeanProject/";

pub(crate) async fn lean_ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<DashboardState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_lean_ws(socket, state))
}

async fn handle_lean_ws(socket: axum::extract::ws::WebSocket, state: Arc<DashboardState>) {
    let project_dir = &state.lean_project_dir;

    let mut child = match Command::new("lake")
        .arg("serve")
        .arg("--")
        .current_dir(project_dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[lean-ws] failed to start lake serve: {e}");
            return;
        }
    };

    let child_stdin = child.stdin.take().unwrap();
    let child_stdout = child.stdout.take().unwrap();
    let (ws_sender, ws_receiver) = socket.split();

    let project_dir_clone = project_dir.clone();
    let stdin_task = tokio::spawn(browser_to_lsp(ws_receiver, child_stdin, project_dir_clone));

    let project_dir_clone = project_dir.clone();
    let stdout_task = tokio::spawn(lsp_to_browser(child_stdout, ws_sender, project_dir_clone));

    tokio::select! {
        _ = stdin_task => {}
        _ = stdout_task => {}
    }

    let _ = child.kill().await;
}

/// Read JSON from the WebSocket, wrap in Content-Length framing, pipe to LSP stdin.
async fn browser_to_lsp(
    mut ws: futures_util::stream::SplitStream<axum::extract::ws::WebSocket>,
    mut stdin: tokio::process::ChildStdin,
    project_dir: std::path::PathBuf,
) {
    while let Some(Ok(msg)) = ws.next().await {
        let text = match msg {
            axum::extract::ws::Message::Text(t) => t,
            axum::extract::ws::Message::Close(_) => break,
            _ => continue,
        };
        let rewritten = rewrite_uris_to_server(&text, &project_dir);
        let header = format!("Content-Length: {}\r\n\r\n", rewritten.len());
        if stdin.write_all(header.as_bytes()).await.is_err() {
            break;
        }
        if stdin.write_all(rewritten.as_bytes()).await.is_err() {
            break;
        }
        let _ = stdin.flush().await;
    }
}

/// Read Content-Length framed JSON from LSP stdout, send as WebSocket text.
async fn lsp_to_browser(
    stdout: tokio::process::ChildStdout,
    mut ws: futures_util::stream::SplitSink<
        axum::extract::ws::WebSocket,
        axum::extract::ws::Message,
    >,
    project_dir: std::path::PathBuf,
) {
    let mut reader = BufReader::new(stdout);
    loop {
        // Read headers until blank line.
        let mut content_length: usize = 0;
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => return,
                _ => {}
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            if let Some(len_str) = trimmed.strip_prefix("Content-Length: ") {
                if let Ok(len) = len_str.parse() {
                    content_length = len;
                }
            }
        }

        if content_length == 0 {
            continue;
        }

        // Read exactly content_length bytes.
        let mut body = vec![0u8; content_length];
        if reader.read_exact(&mut body).await.is_err() {
            return;
        }

        let text = match String::from_utf8(body) {
            Ok(t) => t,
            Err(_) => continue,
        };

        let rewritten = rewrite_uris_to_client(&text, &project_dir);
        if ws
            .send(axum::extract::ws::Message::Text(rewritten.into()))
            .await
            .is_err()
        {
            return;
        }
    }
}

fn rewrite_uris_to_server(json_text: &str, project_dir: &Path) -> String {
    let server_prefix = format!("file://{}/", project_dir.display());
    json_text.replace(VIRTUAL_PREFIX, &server_prefix)
}

fn rewrite_uris_to_client(json_text: &str, project_dir: &Path) -> String {
    let server_prefix = format!("file://{}/", project_dir.display());
    json_text.replace(&server_prefix, VIRTUAL_PREFIX)
}
