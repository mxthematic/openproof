//! Push completed conversation turns into terminal scrollback above the viewport.
//!
//! Uses DECSTBM (Set Top and Bottom Margins) scroll regions to insert lines
//! above the ratatui viewport without disturbing it.

use std::fmt;
use std::io;
use std::io::Write;

use crossterm::cursor::MoveTo;
use crossterm::queue;
use crossterm::style::{Colors, Print, SetAttribute, SetColors};
use crossterm::terminal::{Clear, ClearType};
use crossterm::Command;
use ratatui::backend::Backend;
use ratatui::layout::Size;
use ratatui::text::Line;

use crate::custom_terminal::CustomTerminal;

/// Insert styled lines above the viewport into terminal scrollback.
///
/// The viewport is pushed down to make room. If the viewport is already at the
/// bottom of the screen, old content above it scrolls into terminal scrollback
/// (which the terminal's native scrollbar handles).
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
    if screen_size.height == 0 || screen_size.width == 0 {
        return Ok(());
    }

    let mut area = terminal.viewport_area;
    let wrap_width = area.width.max(1) as usize;

    // Count how many visual rows the content will take.
    let mut content_rows: u16 = 0;
    for line in &lines {
        let w = line.width().max(1);
        content_rows += (w.div_ceil(wrap_width)) as u16;
    }

    if content_rows == 0 {
        return Ok(());
    }

    let last_cursor_pos = terminal.last_known_cursor_pos;

    // We need to make room above the viewport for the content.
    // If viewport doesn't fill the screen, we can push it down.
    // If viewport is at the bottom, content above scrolls into terminal scrollback.

    // First: ensure viewport doesn't start at row 0 with no room.
    // We shrink the viewport from the top and push it down.
    let available_above = area.top();
    let need_above = content_rows;

    if available_above < need_above {
        // Need to push viewport down to make room.
        let push_by = need_above - available_above;
        let push_by = push_by.min(area.height.saturating_sub(3)); // keep at least 3 rows for viewport

        if push_by > 0 {
            // Scroll the viewport region down by push_by rows.
            // This creates empty rows above the viewport.
            let writer = terminal.backend_mut();
            // Set scroll region to entire screen.
            queue!(writer, SetScrollRegion(1..screen_size.height))?;
            // Position cursor at top of viewport.
            queue!(writer, MoveTo(0, area.top()))?;
            // Reverse Index push_by times (pushes content down, creating space at top).
            for _ in 0..push_by {
                queue!(writer, Print("\x1bM"))?;
            }
            queue!(writer, ResetScrollRegion)?;
            io::Write::flush(writer)?;

            area.y += push_by;
            area.height -= push_by;
            terminal.set_viewport_area(area);
        }
    }

    // Now we have space above the viewport. Insert lines there.
    if area.top() == 0 {
        // Still no room -- just skip. This shouldn't happen with push_by > 0.
        return Ok(());
    }

    let writer = terminal.backend_mut();

    // Set scroll region to the area above the viewport.
    queue!(writer, SetScrollRegion(1..area.top()))?;
    // Position cursor at bottom of the history region.
    queue!(writer, MoveTo(0, area.top().saturating_sub(1)))?;

    for line in &lines {
        queue!(writer, Print("\r\n"))?;
        queue!(writer, Clear(ClearType::UntilNewLine))?;
        write_line_spans(writer, line)?;
    }

    queue!(writer, ResetScrollRegion)?;

    // Restore cursor.
    queue!(writer, MoveTo(last_cursor_pos.x, last_cursor_pos.y))?;
    queue!(
        writer,
        SetAttribute(crossterm::style::Attribute::Reset)
    )?;
    io::Write::flush(writer)?;

    // Force full redraw of the viewport since we've changed the scroll region.
    terminal.clear()?;

    Ok(())
}

fn write_line_spans(writer: &mut impl Write, line: &Line) -> io::Result<()> {
    use crossterm::style::Color as CColor;

    // Reset at line start.
    queue!(writer, SetAttribute(crossterm::style::Attribute::Reset))?;

    for span in &line.spans {
        let fg = span.style.fg.map(Into::into).unwrap_or(CColor::Reset);
        let bg = span.style.bg.map(Into::into).unwrap_or(CColor::Reset);
        queue!(writer, SetColors(Colors::new(fg, bg)))?;

        if span
            .style
            .add_modifier
            .contains(ratatui::style::Modifier::BOLD)
        {
            queue!(writer, SetAttribute(crossterm::style::Attribute::Bold))?;
        }
        if span
            .style
            .add_modifier
            .contains(ratatui::style::Modifier::DIM)
        {
            queue!(writer, SetAttribute(crossterm::style::Attribute::Dim))?;
        }
        if span
            .style
            .add_modifier
            .contains(ratatui::style::Modifier::ITALIC)
        {
            queue!(writer, SetAttribute(crossterm::style::Attribute::Italic))?;
        }

        queue!(writer, Print(span.content.as_ref()))?;

        // Reset after each span to prevent style leaking.
        queue!(writer, SetAttribute(crossterm::style::Attribute::Reset))?;
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
