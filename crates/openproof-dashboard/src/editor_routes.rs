//! HTTP routes for the Lean 4 IDE editor page.
//!
//! Serves the Vite-built lean4monaco editor from the `static/editor-dist/`
//! directory and handles file read/write for session workspaces.

use axum::{
    extract::{Path as AxumPath, Query, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;

use crate::DashboardState;

/// Resolve the editor-dist directory. Looks relative to the executable first,
/// then falls back to common development paths.
fn editor_dist_dir() -> Option<PathBuf> {
    // In dev: the static dir is at crates/openproof-dashboard/static/editor-dist/
    // In release: we look relative to the current exe.
    let candidates = [
        // Relative to CWD (dev builds).
        PathBuf::from("crates/openproof-dashboard/static/editor-dist"),
        // Relative to executable.
        std::env::current_exe().ok()?.parent()?.join("editor-dist"),
    ];
    candidates
        .into_iter()
        .find(|p| p.join("index.html").exists())
}

pub(crate) async fn editor_page() -> Response {
    let Some(dist) = editor_dist_dir() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Editor not built. Run scripts/build-editor.sh first.",
        )
            .into_response();
    };
    match std::fs::read_to_string(dist.join("index.html")) {
        Ok(html) => Html(html).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

pub(crate) async fn editor_asset(AxumPath(path): AxumPath<String>) -> Response {
    // Prevent directory traversal.
    if path.contains("..") {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(dist) = editor_dist_dir() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let file_path = dist.join("assets").join(&path);
    serve_static_file(&file_path).await
}

pub(crate) async fn editor_infoview_asset(AxumPath(path): AxumPath<String>) -> Response {
    if path.contains("..") {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(dist) = editor_dist_dir() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let file_path = dist.join("infoview").join(&path);
    serve_static_file(&file_path).await
}

async fn serve_static_file(path: &std::path::Path) -> Response {
    let content_type = match path.extension().and_then(|e| e.to_str()) {
        Some("js") => "application/javascript",
        Some("css") => "text/css",
        Some("json") => "application/json",
        Some("wasm") => "application/wasm",
        Some("ttf") => "font/ttf",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    };
    match std::fs::read(path) {
        Ok(bytes) => ([(header::CONTENT_TYPE, content_type)], bytes).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Deserialize)]
pub(crate) struct FileQuery {
    id: String,
    #[serde(default = "default_scratch")]
    path: String,
}

fn default_scratch() -> String {
    "Scratch.lean".to_string()
}

pub(crate) async fn read_file(
    Query(q): Query<FileQuery>,
    State(state): State<Arc<DashboardState>>,
) -> Response {
    if q.path.contains("..") || q.path.starts_with('/') {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let file_path = state.store.workspace_dir(&q.id).join(&q.path);
    match std::fs::read_to_string(&file_path) {
        Ok(content) => Json(serde_json::json!({ "content": content })).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Deserialize)]
pub(crate) struct WriteFileBody {
    id: String,
    #[serde(default = "default_scratch")]
    path: String,
    content: String,
}

pub(crate) async fn write_file(
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<WriteFileBody>,
) -> Response {
    if body.path.contains("..") || body.path.starts_with('/') {
        return StatusCode::BAD_REQUEST.into_response();
    }
    match state
        .store
        .write_workspace_file(&body.id, &body.path, &body.content)
    {
        Ok(_) => StatusCode::OK.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
