//! Push completed conversation turns into terminal scrollback above the viewport.
//!
//! Uses DECSTBM (Set Top and Bottom Margins) scroll regions to insert lines
//! above the ratatui viewport without disturbing it. Content pushed this way
//! enters the terminal emulator's native scrollback buffer, enabling the
//! native scrollbar.

use std::fmt;
use std::io;
use std::io::Write;

use crossterm::cursor::MoveTo;
use crossterm::queue;
use crossterm::style::{Color as CColor, Colors, Print, SetAttribute, SetColors};
use crossterm::terminal::{Clear, ClearType};
use crossterm::Command;
use ratatui::backend::Backend;
use ratatui::layout::Size;
use ratatui::text::Line;

use crate::custom_terminal::CustomTerminal;

/// Insert styled lines above the viewport into terminal scrollback.
pub fn insert_history_lines<B>(
    terminal: &mut CustomTerminal<B>,
    lines: Vec<Line>,
) -> io::Result<()>
where
    B: Backend + Write,
{
    if lines.is_empty() {
        return Ok(());
    }

    let screen_size = terminal.size().unwrap_or(Size::new(0, 0));
    let mut area = terminal.viewport_area;
    let mut should_update_area = false;
    let last_cursor_pos = terminal.last_known_cursor_pos;
    let writer = terminal.backend_mut();

    // Simple wrapping: count visual rows at terminal width.
    let wrap_width = area.width.max(1) as usize;
    let mut wrapped_rows: u16 = 0;
    for line in &lines {
        let width = line.width().max(1);
        wrapped_rows += (width.div_ceil(wrap_width)) as u16;
    }

    let cursor_top = if area.bottom() < screen_size.height {
        // Viewport not at bottom: scroll it down to make room.
        let scroll_amount = wrapped_rows.min(screen_size.height - area.bottom());
        queue!(writer, SetScrollRegion(area.top() + 1..screen_size.height))?;
        queue!(writer, MoveTo(0, area.top()))?;
        for _ in 0..scroll_amount {
            queue!(writer, Print("\x1bM"))?; // Reverse Index
        }
        queue!(writer, ResetScrollRegion)?;
        let ct = area.top().saturating_sub(1);
        area.y += scroll_amount;
        should_update_area = true;
        ct
    } else {
        area.top().saturating_sub(1)
    };

    // Set scroll region to history area (above viewport).
    queue!(writer, SetScrollRegion(1..area.top()))?;
    queue!(writer, MoveTo(0, cursor_top))?;

    for line in &lines {
        queue!(writer, Print("\r\n"))?;
        queue!(writer, Clear(ClearType::UntilNewLine))?;
        write_line_spans(writer, line)?;
    }

    queue!(writer, ResetScrollRegion)?;

    // Restore cursor.
    queue!(writer, MoveTo(last_cursor_pos.x, last_cursor_pos.y))?;

    let _ = writer;
    if should_update_area {
        terminal.set_viewport_area(area);
    }

    Ok(())
}

fn write_line_spans(writer: &mut impl Write, line: &Line) -> io::Result<()> {
    for span in &line.spans {
        let fg = span
            .style
            .fg
            .map(Into::into)
            .unwrap_or(CColor::Reset);
        let bg = span
            .style
            .bg
            .map(Into::into)
            .unwrap_or(CColor::Reset);
        queue!(writer, SetColors(Colors::new(fg, bg)))?;

        if span.style.add_modifier.contains(ratatui::style::Modifier::BOLD) {
            queue!(
                writer,
                SetAttribute(crossterm::style::Attribute::Bold)
            )?;
        }
        if span.style.add_modifier.contains(ratatui::style::Modifier::DIM) {
            queue!(
                writer,
                SetAttribute(crossterm::style::Attribute::Dim)
            )?;
        }

        queue!(writer, Print(span.content.as_ref()))?;

        queue!(
            writer,
            SetAttribute(crossterm::style::Attribute::Reset)
        )?;
    }
    Ok(())
}

// --- ANSI scroll region commands ---

#[derive(Debug, Clone)]
pub struct SetScrollRegion(pub std::ops::Range<u16>);

impl Command for SetScrollRegion {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[{};{}r", self.0.start, self.0.end)
    }
    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct ResetScrollRegion;

impl Command for ResetScrollRegion {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[r")
    }
    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        Ok(())
    }
}
