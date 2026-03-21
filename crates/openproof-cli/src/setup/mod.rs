//! First-run setup wizard for openproof.
//!
//! Two steps: model provider selection and corpus mode.
//! Writes ~/.openproof/config.json on completion.

mod app;
mod ui;

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event as CrosstermEvent, KeyEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

pub use app::{SetupApp, SetupResult};

fn config_dir() -> PathBuf {
    dirs_home().join(".openproof")
}

fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

fn dirs_home() -> PathBuf {
    directories::BaseDirs::new()
        .map(|b| b.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Check if setup has been completed.
pub fn is_setup_complete() -> bool {
    let path = config_path();
    if !path.exists() {
        return false;
    }
    match std::fs::read_to_string(&path) {
        Ok(content) => {
            serde_json::from_str::<serde_json::Value>(&content)
                .ok()
                .and_then(|v| v.get("setup_complete")?.as_bool())
                .unwrap_or(false)
        }
        Err(_) => false,
    }
}

/// Load the setup config.
pub fn load_config() -> Option<SetupResult> {
    let content = std::fs::read_to_string(config_path()).ok()?;
    serde_json::from_str(&content).ok()
}

/// Write the setup config.
pub fn save_config(result: &SetupResult) -> Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    let content = serde_json::to_string_pretty(result)?;
    std::fs::write(config_path(), content)?;
    Ok(())
}

/// Run the setup wizard. Returns the result or None if cancelled.
pub fn run_wizard() -> Result<Option<SetupResult>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    // Clear screen for the wizard.
    io::Write::write_all(&mut stdout, b"\x1b[2J\x1b[H")?;
    io::Write::flush(&mut stdout)?;

    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;

    let mut app = SetupApp::new();
    let tick_rate = Duration::from_millis(50);

    while app.running {
        terminal.draw(|f| ui::draw(f, &app))?;

        if event::poll(tick_rate)? {
            if let CrosstermEvent::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    app.handle_key(key);
                }
            }
        }
    }

    disable_raw_mode()?;
    // Clear screen after wizard.
    let stdout = terminal.backend_mut();
    io::Write::write_all(stdout, b"\x1b[2J\x1b[H")?;
    io::Write::flush(stdout)?;
    drop(terminal);

    if app.cancelled {
        Ok(None)
    } else {
        Ok(Some(app.result()))
    }
}
