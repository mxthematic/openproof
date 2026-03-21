use chrono::Utc;
use openproof_protocol::{
    MessageRole, ProofQuestionOption, ProofQuestionState, SessionSnapshot, ShareMode,
    TranscriptEntry,
};

use crate::helpers::{default_session_with_workspace, next_id, share_mode_label};
use crate::state::{AppState, PendingWrite, SubmittedInput};

impl AppState {
    pub fn current_session(&self) -> Option<&SessionSnapshot> {
        self.sessions.get(self.selected_session)
    }

    pub fn current_session_mut(&mut self) -> Option<&mut SessionSnapshot> {
        self.sessions.get_mut(self.selected_session)
    }

    pub fn active_proof_node(&self) -> Option<&openproof_protocol::ProofNode> {
        let session = self.current_session()?;
        let active_id = session.proof.active_node_id.as_deref()?;
        session.proof.nodes.iter().find(|node| node.id == active_id)
    }

    pub fn pending_question(&self) -> Option<&ProofQuestionState> {
        self.current_session()?.proof.pending_question.as_ref()
    }

    pub fn has_open_question(&self) -> bool {
        self.pending_question()
            .map(|question| {
                question.status != "resolved" && !question.options.is_empty()
            })
            .unwrap_or(false)
    }

    pub fn selected_question_option(&self) -> Option<&ProofQuestionOption> {
        let question = self.pending_question()?;
        if question.options.is_empty() {
            return None;
        }
        let index = self
            .selected_question_option
            .min(question.options.len().saturating_sub(1));
        question.options.get(index)
    }

    pub fn sync_question_selection(&mut self) {
        let Some(question) = self.pending_question() else {
            self.selected_question_option = 0;
            return;
        };
        if question.options.is_empty() {
            self.selected_question_option = 0;
            return;
        }
        if let Some(recommended) = question.recommended_option_id.as_ref() {
            if let Some(index) = question
                .options
                .iter()
                .position(|option| &option.id == recommended)
            {
                self.selected_question_option = index;
                return;
            }
        }
        self.selected_question_option = self
            .selected_question_option
            .min(question.options.len().saturating_sub(1));
    }

    pub fn submit_composer(&mut self) -> Option<SubmittedInput> {
        let text = self.composer.trim().to_string();
        self.composer.clear();
        self.composer_cursor = 0;
        self.submit_text(text)
    }

    pub fn submit_text(&mut self, text: String) -> Option<SubmittedInput> {
        let text = text.trim().to_string();
        if text.is_empty() {
            return None;
        }
        let now = Utc::now().to_rfc3339();
        let entry = TranscriptEntry {
            id: next_id("native_msg"),
            role: MessageRole::User,
            title: None,
            content: text.clone(),
            created_at: now.clone(),
        };
        if let Some(session) = self.current_session_mut() {
            session.updated_at = now;
            if let Some(question) = session.proof.pending_question.as_mut() {
                if question.status != "resolved" {
                    question.answer_text = Some(text.clone());
                    question.status = "answered".to_string();
                }
            }
            session.transcript.push(entry.clone());
            let session_snapshot = session.clone();
            let submitted = SubmittedInput {
                session_id: session.id.clone(),
                raw_text: text,
                user_entry: entry,
                session_snapshot,
            };
            self.pending_writes += 1;
            return Some(submitted);
        }
        None
    }

    pub fn create_session(&mut self, title: Option<&str>) -> PendingWrite {
        let mut session = default_session_with_workspace(
            self.workspace_root.as_deref(),
            self.workspace_label.as_deref(),
        );
        if let Some(title) = title {
            let trimmed = title.trim();
            if !trimmed.is_empty() {
                session.title = trimmed.to_string();
            }
        }
        self.sessions.insert(0, session.clone());
        self.selected_session = 0;
        self.scroll_offset = 0;
        self.flushed_turn_count = 0;
        self.selected_question_option = 0;
        self.pending_writes += 1;
        self.status = format!("Started session {}.", session.title);
        PendingWrite { session }
    }

    pub fn switch_session(&mut self, session_id: &str) -> Result<(), String> {
        let Some(index) = self
            .sessions
            .iter()
            .position(|session| session.id == session_id)
        else {
            return Err(format!("Session not found: {session_id}"));
        };
        self.selected_session = index;
        self.scroll_offset = 0;
        self.flushed_turn_count = 0;
        self.sync_question_selection();
        let title = self.sessions[index].title.clone();
        self.status = format!("Resumed {title}.");
        Ok(())
    }

    pub fn set_share_mode(&mut self, share_mode: ShareMode) -> Result<PendingWrite, String> {
        let timestamp = Utc::now().to_rfc3339();
        let snapshot = {
            let Some(session) = self.current_session_mut() else {
                return Err("No active session.".to_string());
            };
            session.updated_at = timestamp;
            session.cloud.share_mode = share_mode;
            if share_mode == ShareMode::Local {
                session.cloud.sync_enabled = false;
            }
            session.clone()
        };
        self.pending_writes += 1;
        self.status = format!("Share mode set to {}.", share_mode_label(share_mode));
        Ok(PendingWrite { session: snapshot })
    }

    pub fn set_sync_enabled(&mut self, enabled: bool) -> Result<PendingWrite, String> {
        let timestamp = Utc::now().to_rfc3339();
        let snapshot = {
            let Some(session) = self.current_session_mut() else {
                return Err("No active session.".to_string());
            };
            if session.cloud.share_mode == ShareMode::Local && enabled {
                return Err(
                    "Set share mode to community or private before enabling sync.".to_string(),
                );
            }
            session.updated_at = timestamp;
            session.cloud.sync_enabled = enabled;
            session.clone()
        };
        self.pending_writes += 1;
        self.status = if enabled {
            "Enabled sync for the current session.".to_string()
        } else {
            "Disabled sync for the current session.".to_string()
        };
        Ok(PendingWrite { session: snapshot })
    }

    pub fn set_private_overlay_community(
        &mut self,
        enabled: bool,
    ) -> Result<PendingWrite, String> {
        let timestamp = Utc::now().to_rfc3339();
        let snapshot = {
            let Some(session) = self.current_session_mut() else {
                return Err("No active session.".to_string());
            };
            session.updated_at = timestamp;
            session.cloud.private_overlay_community = enabled;
            session.clone()
        };
        self.pending_writes += 1;
        self.status = if enabled {
            "Private share mode will also search the community overlay.".to_string()
        } else {
            "Private share mode will stay isolated from community results.".to_string()
        };
        Ok(PendingWrite { session: snapshot })
    }

    pub fn mark_sync_completed(&mut self) -> Result<PendingWrite, String> {
        let timestamp = Utc::now().to_rfc3339();
        let snapshot = {
            let Some(session) = self.current_session_mut() else {
                return Err("No active session.".to_string());
            };
            session.updated_at = timestamp.clone();
            session.cloud.last_sync_at = Some(timestamp);
            session.clone()
        };
        self.pending_writes += 1;
        self.status = "Shared corpus sync completed.".to_string();
        Ok(PendingWrite { session: snapshot })
    }
}
