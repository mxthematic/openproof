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
}
