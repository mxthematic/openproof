use anyhow::{Context, Result};
use directories::BaseDirs;
use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};

use crate::schema::open_connection;

#[derive(Debug, Clone)]
pub struct StorePaths {
    pub db_path: PathBuf,
    pub legacy_sessions_dir: PathBuf,
    pub sessions_dir: PathBuf,
}

impl StorePaths {
    pub fn detect() -> Result<Self> {
        let base_dirs = BaseDirs::new().context("could not resolve home directory")?;
        let home = base_dirs.home_dir().join(".openproof");
        Ok(Self {
            db_path: home.join("native").join("openproof-native.sqlite"),
            legacy_sessions_dir: home.join("sessions"),
            sessions_dir: home.join("workspaces"),
        })
    }
}

#[derive(Debug, Clone)]
pub struct AppStore {
    pub(crate) paths: StorePaths,
}

impl AppStore {
    pub fn open(paths: StorePaths) -> Result<Self> {
        if let Some(parent) = paths.db_path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let store = Self { paths };
        store.init_schema()?;
        Ok(store)
    }

    pub fn paths(&self) -> &StorePaths {
        &self.paths
    }

    pub fn db_path(&self) -> &Path {
        &self.paths.db_path
    }

    pub(crate) fn connect(&self) -> Result<Connection> {
        open_connection(&self.paths.db_path)
    }

    fn init_schema(&self) -> Result<()> {
        let _ = self.connect()?;
        Ok(())
    }

    /// Open a raw connection for bulk operations (used by corpus crate).
    pub fn connect_for_bulk(&self) -> Result<Connection> {
        self.connect()
    }

    // --- Persistent session workspace ---

    /// Get the directory for a session's persistent files.
    /// Creates it if it doesn't exist.
    pub fn session_dir(&self, session_id: &str) -> Result<PathBuf> {
        let dir = self.paths.sessions_dir.join(session_id);
        fs::create_dir_all(&dir)
            .with_context(|| format!("creating session dir {}", dir.display()))?;
        fs::create_dir_all(dir.join("history"))
            .with_context(|| format!("creating history dir for {session_id}"))?;
        Ok(dir)
    }

    /// Write the Scratch.lean file for a session.
    /// Archives the previous version to history/NNN_attempt.lean first.
    /// Write a patch diff alongside the attempt archive.
    pub fn write_patch_diff(&self, session_id: &str, attempt_number: usize, diff: &str) -> Result<()> {
        let dir = self.session_dir(session_id)?;
        let history_dir = dir.join("history");
        let diff_path = history_dir.join(format!("{:03}_patch.diff", attempt_number));
        fs::write(diff_path, diff)?;
        Ok(())
    }

    pub fn write_scratch(&self, session_id: &str, content: &str) -> Result<(PathBuf, usize)> {
        let dir = self.session_dir(session_id)?;
        let scratch_path = dir.join("Scratch.lean");
        let history_dir = dir.join("history");

        // Count existing attempts to determine the next number
        let existing: Vec<_> = fs::read_dir(&history_dir)
            .ok()
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        e.file_name()
                            .to_string_lossy()
                            .ends_with("_attempt.lean")
                    })
                    .collect()
            })
            .unwrap_or_default();
        let attempt_number = existing.len() + 1;

        // Archive current scratch if it exists
        if scratch_path.exists() {
            let archive_name = format!("{:03}_attempt.lean", attempt_number);
            let archive_path = history_dir.join(archive_name);
            fs::copy(&scratch_path, &archive_path)?;
        }

        // Write new scratch
        fs::write(&scratch_path, content)?;
        Ok((scratch_path, attempt_number))
    }

    /// Write the Paper.tex file for a session.
    pub fn write_paper(&self, session_id: &str, content: &str) -> Result<PathBuf> {
        let dir = self.session_dir(session_id)?;
        let paper_path = dir.join("Paper.tex");
        fs::write(&paper_path, content)?;
        Ok(paper_path)
    }

    /// Read the current Scratch.lean content for a session.
    pub fn read_scratch(&self, session_id: &str) -> Option<String> {
        let path = self.paths.sessions_dir.join(session_id).join("Scratch.lean");
        fs::read_to_string(path).ok()
    }

    /// List all history attempt files for a session, sorted.
    pub fn list_scratch_history(&self, session_id: &str) -> Vec<PathBuf> {
        let history_dir = self.paths.sessions_dir.join(session_id).join("history");
        let mut files: Vec<PathBuf> = fs::read_dir(&history_dir)
            .ok()
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.extension().map(|e| e == "lean").unwrap_or(false))
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        files
    }

    /// Get the workspace directory path for a session (without creating it).
    pub fn workspace_dir(&self, session_id: &str) -> PathBuf {
        self.paths.sessions_dir.join(session_id)
    }

    /// List all files in the session workspace directory.
    /// Returns (relative_path, size_in_bytes) pairs.
    pub fn list_workspace_files(&self, session_id: &str) -> Result<Vec<(String, u64)>> {
        let dir = self.workspace_dir(session_id);
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut files = Vec::new();
        Self::walk_workspace(&dir, &dir, &mut files)?;
        files.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(files)
    }

    fn walk_workspace(
        base: &Path,
        current: &Path,
        out: &mut Vec<(String, u64)>,
    ) -> Result<()> {
        let entries = fs::read_dir(current)
            .with_context(|| format!("reading workspace dir {}", current.display()))?;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            // Skip the history directory.
            if path.is_dir() {
                let name = path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                // Skip build artifacts, history, hidden dirs, and package dirs
                if name == "history"
                    || name == ".lake"
                    || name == ".git"
                    || name.starts_with('.')
                    || name == "build"
                    || name == "lake-packages"
                {
                    continue;
                }
                Self::walk_workspace(base, &path, out)?;
            } else {
                let relative = path
                    .strip_prefix(base)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string();
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                out.push((relative, size));
            }
        }
        Ok(())
    }

    /// Read a file from the session workspace.
    /// Returns an error if the path escapes the workspace directory.
    pub fn read_workspace_file(&self, session_id: &str, relative_path: &str) -> Result<String> {
        let dir = self.workspace_dir(session_id);
        let target = sanitize_workspace_path(&dir, relative_path)?;
        fs::read_to_string(&target)
            .with_context(|| format!("reading workspace file {}", target.display()))
    }

    /// Write a file to the session workspace.
    /// Creates parent directories as needed.
    /// Returns an error if the path escapes the workspace directory.
    pub fn write_workspace_file(
        &self,
        session_id: &str,
        relative_path: &str,
        content: &str,
    ) -> Result<PathBuf> {
        let dir = self.session_dir(session_id)?;
        let target = sanitize_workspace_path(&dir, relative_path)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating parent dirs for {}", target.display()))?;
        }
        fs::write(&target, content)
            .with_context(|| format!("writing workspace file {}", target.display()))?;
        Ok(target)
    }
}

/// Validate that a relative path stays within the workspace directory.
/// Rejects absolute paths and `..` traversal.
fn sanitize_workspace_path(workspace_dir: &Path, relative_path: &str) -> Result<PathBuf> {
    let relative = Path::new(relative_path);
    anyhow::ensure!(
        relative.is_relative(),
        "workspace path must be relative, got: {relative_path}"
    );
    // Reject any component that is ".."
    for component in relative.components() {
        if matches!(component, std::path::Component::ParentDir) {
            anyhow::bail!("workspace path must not contain '..': {relative_path}");
        }
    }
    let target = workspace_dir.join(relative);
    Ok(target)
}
