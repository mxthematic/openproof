use crate::commands::delete_word_backward_pos;
use crate::state::{AppState, FocusPane};

impl AppState {
    pub(crate) fn apply_input_char(&mut self, ch: char) {
        if self.focus == FocusPane::Composer {
            self.composer.insert(self.composer_cursor, ch);
            self.composer_cursor += ch.len_utf8();
        }
    }

    pub(crate) fn apply_backspace(&mut self) {
        if self.focus == FocusPane::Composer && self.composer_cursor > 0 {
            let prev = self.composer[..self.composer_cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.composer.remove(prev);
            self.composer_cursor = prev;
        }
    }

    pub(crate) fn apply_cursor_left(&mut self) {
        if self.focus == FocusPane::Composer && self.composer_cursor > 0 {
            self.composer_cursor = self.composer[..self.composer_cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
        }
    }

    pub(crate) fn apply_cursor_right(&mut self) {
        if self.focus == FocusPane::Composer && self.composer_cursor < self.composer.len() {
            self.composer_cursor = self.composer[self.composer_cursor..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| self.composer_cursor + i)
                .unwrap_or(self.composer.len());
        }
    }

    pub(crate) fn apply_cursor_home(&mut self) {
        if self.focus == FocusPane::Composer {
            self.composer_cursor = 0;
        }
    }

    pub(crate) fn apply_cursor_end(&mut self) {
        if self.focus == FocusPane::Composer {
            self.composer_cursor = self.composer.len();
        }
    }

    pub(crate) fn apply_delete_forward(&mut self) {
        if self.focus == FocusPane::Composer && self.composer_cursor < self.composer.len() {
            self.composer.remove(self.composer_cursor);
        }
    }

    pub(crate) fn apply_delete_word_backward(&mut self) {
        if self.focus == FocusPane::Composer && self.composer_cursor > 0 {
            let new_pos = delete_word_backward_pos(&self.composer, self.composer_cursor);
            self.composer.drain(new_pos..self.composer_cursor);
            self.composer_cursor = new_pos;
        }
    }

    pub(crate) fn apply_clear_to_start(&mut self) {
        if self.focus == FocusPane::Composer {
            self.composer.drain(..self.composer_cursor);
            self.composer_cursor = 0;
        }
    }

    pub(crate) fn apply_paste(&mut self, text: String) {
        if self.focus == FocusPane::Composer {
            self.composer.insert_str(self.composer_cursor, &text);
            self.composer_cursor += text.len();
        }
    }

}
