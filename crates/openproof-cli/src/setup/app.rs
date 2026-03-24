//! Setup wizard state machine.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Provider,
    Corpus,
    Finish,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupResult {
    pub setup_complete: bool,
    pub model_provider: String,
    pub api_key: Option<String>,
    pub corpus_mode: String,
    pub corpus_url: Option<String>,
}

pub struct SetupApp {
    pub running: bool,
    pub cancelled: bool,
    pub step: Step,
    pub provider_selected: usize,
    pub corpus_selected: usize,
    pub api_key_input: String,
    pub api_key_cursor: usize,
    pub entering_key: bool,
}

pub const PROVIDERS: &[(&str, &str, bool)] = &[
    ("codex", "Codex (ChatGPT) -- uses existing openproof login", false),
    ("openai", "OpenAI API -- requires OPENAI_API_KEY", true),
    ("anthropic", "Anthropic -- requires ANTHROPIC_API_KEY", true),
];

pub const CORPUS_MODES: &[(&str, &str)] = &[
    ("cloud", "Cloud (recommended) -- faster proofs, larger search corpus"),
    ("local", "Local only -- auto-imports Mathlib, no network"),
];

impl SetupApp {
    pub fn new() -> Self {
        Self {
            running: true,
            cancelled: false,
            step: Step::Provider,
            provider_selected: 0,
            corpus_selected: 0,
            api_key_input: String::new(),
            api_key_cursor: 0,
            entering_key: false,
        }
    }

    pub fn result(&self) -> SetupResult {
        let (provider_id, _, _) = PROVIDERS[self.provider_selected];
        let (corpus_id, _) = CORPUS_MODES[self.corpus_selected];
        SetupResult {
            setup_complete: true,
            model_provider: provider_id.to_string(),
            api_key: if self.api_key_input.is_empty() {
                None
            } else {
                Some(self.api_key_input.clone())
            },
            corpus_mode: corpus_id.to_string(),
            corpus_url: if corpus_id == "cloud" {
                Some("https://openproof-cloud-production.up.railway.app".to_string())
            } else {
                None
            },
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.cancelled = true;
            self.running = false;
            return;
        }
        if key.code == KeyCode::Esc {
            if self.entering_key {
                self.entering_key = false;
                return;
            }
            match self.step {
                Step::Provider => {
                    self.cancelled = true;
                    self.running = false;
                }
                Step::Corpus => self.step = Step::Provider,
                Step::Finish => self.step = Step::Corpus,
            }
            return;
        }

        match self.step {
            Step::Provider => self.handle_provider_key(key),
            Step::Corpus => self.handle_corpus_key(key),
            Step::Finish => {
                if key.code == KeyCode::Enter {
                    self.running = false;
                }
            }
        }
    }

    fn handle_provider_key(&mut self, key: KeyEvent) {
        if self.entering_key {
            match key.code {
                KeyCode::Enter => {
                    if !self.api_key_input.is_empty() {
                        self.entering_key = false;
                        self.step = Step::Corpus;
                    }
                }
                KeyCode::Backspace => {
                    if self.api_key_cursor > 0 {
                        let prev = self.api_key_input[..self.api_key_cursor]
                            .char_indices()
                            .next_back()
                            .map(|(i, _)| i)
                            .unwrap_or(0);
                        self.api_key_input.remove(prev);
                        self.api_key_cursor = prev;
                    }
                }
                KeyCode::Char(c) => {
                    self.api_key_input.insert(self.api_key_cursor, c);
                    self.api_key_cursor += c.len_utf8();
                }
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Up => {
                self.provider_selected = self.provider_selected.saturating_sub(1);
            }
            KeyCode::Down => {
                if self.provider_selected + 1 < PROVIDERS.len() {
                    self.provider_selected += 1;
                }
            }
            KeyCode::Enter => {
                let (_, _, needs_key) = PROVIDERS[self.provider_selected];
                if needs_key {
                    self.entering_key = true;
                    self.api_key_input.clear();
                    self.api_key_cursor = 0;
                } else {
                    self.step = Step::Corpus;
                }
            }
            _ => {}
        }
    }

    fn handle_corpus_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => {
                self.corpus_selected = self.corpus_selected.saturating_sub(1);
            }
            KeyCode::Down => {
                if self.corpus_selected + 1 < CORPUS_MODES.len() {
                    self.corpus_selected += 1;
                }
            }
            KeyCode::Enter => {
                self.step = Step::Finish;
            }
            _ => {}
        }
    }
}
