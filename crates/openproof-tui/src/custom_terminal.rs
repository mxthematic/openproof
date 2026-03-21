// Adapted from Codex TUI's custom_terminal.rs (MIT licensed, see original).
// Provides an inline-viewport terminal that renders into the normal terminal
// buffer instead of the alternate screen, enabling the terminal emulator's
// native scrollbar.

use std::io;
use std::io::Write;

use crossterm::cursor::MoveTo;
use crossterm::queue;
use crossterm::style::{
    Colors, Print, SetAttribute, SetBackgroundColor, SetColors, SetForegroundColor,
};
use crossterm::terminal::Clear;
use ratatui::backend::Backend;
use ratatui::backend::ClearType;
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::{Position, Rect, Size};
use ratatui::style::{Color, Modifier};
use unicode_width::UnicodeWidthStr;

/// Display width of a cell symbol, ignoring OSC escape sequences.
fn display_width(s: &str) -> usize {
    if !s.contains('\x1B') {
        return s.width();
    }
    let mut visible = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1B' && chars.clone().next() == Some(']') {
            chars.next();
            for c in chars.by_ref() {
                if c == '\x07' {
                    break;
                }
            }
            continue;
        }
        visible.push(ch);
    }
    visible.width()
}

pub struct Frame<'a> {
    pub(crate) cursor_position: Option<Position>,
    pub(crate) viewport_area: Rect,
    pub(crate) buffer: &'a mut Buffer,
}

impl Frame<'_> {
    pub const fn area(&self) -> Rect {
        self.viewport_area
    }

    pub fn render_widget<W: ratatui::widgets::Widget>(&mut self, widget: W, area: Rect) {
        widget.render(area, self.buffer);
    }

    pub fn render_stateful_widget<W: ratatui::widgets::StatefulWidget>(
        &mut self,
        widget: W,
        area: Rect,
        state: &mut W::State,
    ) {
        widget.render(area, self.buffer, state);
    }

    pub fn set_cursor_position<P: Into<Position>>(&mut self, position: P) {
        self.cursor_position = Some(position.into());
    }

    pub fn buffer_mut(&mut self) -> &mut Buffer {
        self.buffer
    }
}

#[derive(Debug, Default, Clone, Eq, PartialEq, Hash)]
pub struct CustomTerminal<B: Backend + Write> {
    backend: B,
    buffers: [Buffer; 2],
    current: usize,
    pub hidden_cursor: bool,
    pub viewport_area: Rect,
    pub last_known_screen_size: Size,
    pub last_known_cursor_pos: Position,
}

impl<B: Backend + Write> Drop for CustomTerminal<B> {
    fn drop(&mut self) {
        if self.hidden_cursor {
            let _ = self.show_cursor();
        }
    }
}

impl<B: Backend + Write> CustomTerminal<B> {
    pub fn with_options(mut backend: B) -> io::Result<Self> {
        let screen_size = backend.size()?;
        let cursor_pos = backend.get_cursor_position().unwrap_or(Position { x: 0, y: 0 });
        Ok(Self {
            backend,
            buffers: [Buffer::empty(Rect::ZERO), Buffer::empty(Rect::ZERO)],
            current: 0,
            hidden_cursor: false,
            viewport_area: Rect::new(0, cursor_pos.y, 0, 0),
            last_known_screen_size: screen_size,
            last_known_cursor_pos: cursor_pos,
        })
    }

    pub fn get_frame(&mut self) -> Frame<'_> {
        Frame {
            cursor_position: None,
            viewport_area: self.viewport_area,
            buffer: &mut self.buffers[self.current],
        }
    }

    fn current_buffer(&self) -> &Buffer {
        &self.buffers[self.current]
    }

    fn current_buffer_mut(&mut self) -> &mut Buffer {
        &mut self.buffers[self.current]
    }

    fn previous_buffer(&self) -> &Buffer {
        &self.buffers[1 - self.current]
    }

    fn previous_buffer_mut(&mut self) -> &mut Buffer {
        &mut self.buffers[1 - self.current]
    }

    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    pub fn flush(&mut self) -> io::Result<()> {
        let updates = diff_buffers(self.previous_buffer(), self.current_buffer());
        if let Some(&DrawCommand::Put { x, y, .. }) = updates.iter().rfind(|c| c.is_put()) {
            self.last_known_cursor_pos = Position { x, y };
        }
        draw(&mut self.backend, updates.into_iter())
    }

    pub fn resize(&mut self, screen_size: Size) -> io::Result<()> {
        self.last_known_screen_size = screen_size;
        Ok(())
    }

    pub fn set_viewport_area(&mut self, area: Rect) {
        self.current_buffer_mut().resize(area);
        self.previous_buffer_mut().resize(area);
        self.viewport_area = area;
    }

    pub fn autoresize(&mut self) -> io::Result<()> {
        let screen_size = self.size()?;
        if screen_size != self.last_known_screen_size {
            self.last_known_screen_size = screen_size;
            // Recompute viewport to fill the terminal from the current y position
            let height = screen_size.height.saturating_sub(self.viewport_area.y);
            let new_area = Rect::new(
                0,
                self.viewport_area.y,
                screen_size.width,
                height,
            );
            self.set_viewport_area(new_area);
        }
        Ok(())
    }

    pub fn draw<F>(&mut self, render_callback: F) -> io::Result<()>
    where
        F: FnOnce(&mut Frame),
    {
        self.autoresize()?;
        let mut frame = self.get_frame();
        render_callback(&mut frame);
        let cursor_position = frame.cursor_position;
        self.flush()?;
        match cursor_position {
            None => self.hide_cursor()?,
            Some(position) => {
                self.show_cursor()?;
                self.set_cursor_position(position)?;
            }
        }
        self.swap_buffers();
        io::Write::flush(&mut self.backend)?;
        Ok(())
    }

    pub fn hide_cursor(&mut self) -> io::Result<()> {
        self.backend.hide_cursor()?;
        self.hidden_cursor = true;
        Ok(())
    }

    pub fn show_cursor(&mut self) -> io::Result<()> {
        self.backend.show_cursor()?;
        self.hidden_cursor = false;
        Ok(())
    }

    pub fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        let position = position.into();
        self.backend.set_cursor_position(position)?;
        self.last_known_cursor_pos = position;
        Ok(())
    }

    pub fn clear(&mut self) -> io::Result<()> {
        if self.viewport_area.is_empty() {
            return Ok(());
        }
        self.backend
            .set_cursor_position(self.viewport_area.as_position())?;
        self.backend.clear_region(ClearType::AfterCursor)?;
        self.previous_buffer_mut().reset();
        Ok(())
    }

    pub fn swap_buffers(&mut self) {
        self.previous_buffer_mut().reset();
        self.current = 1 - self.current;
    }

    pub fn size(&self) -> io::Result<Size> {
        self.backend.size()
    }

    /// Push lines into terminal scrollback by scrolling the viewport area up.
    /// This is what makes the native scrollbar work -- content moves from the
    /// viewport into the terminal's scrollback buffer.
    pub fn scroll_region_up(
        &mut self,
        rows_to_scroll: u16,
    ) -> io::Result<()> {
        if rows_to_scroll == 0 || self.viewport_area.is_empty() {
            return Ok(());
        }
        // Set scroll region to the viewport area
        let top = self.viewport_area.y;
        let bottom = self.viewport_area.bottom().saturating_sub(1);
        write!(self.backend, "\x1b[{};{}r", top + 1, bottom + 1)?;
        // Move cursor to top of region and scroll up
        write!(self.backend, "\x1b[{};1H", top + 1)?;
        for _ in 0..rows_to_scroll {
            write!(self.backend, "\x1bM")?; // Reverse Index (scroll down = push content up)
        }
        // Reset scroll region to full terminal
        write!(self.backend, "\x1b[r")?;
        io::Write::flush(&mut self.backend)?;
        self.previous_buffer_mut().reset();
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Diff-based rendering (ported from Codex)
// ---------------------------------------------------------------------------

enum DrawCommand {
    Put { x: u16, y: u16, cell: Cell },
    ClearToEnd { x: u16, y: u16, bg: Color },
}

impl DrawCommand {
    fn is_put(&self) -> bool {
        matches!(self, Self::Put { .. })
    }
}

fn diff_buffers(a: &Buffer, b: &Buffer) -> Vec<DrawCommand> {
    let previous_buffer = &a.content;
    let next_buffer = &b.content;
    let mut updates = vec![];
    let mut last_nonblank_columns = vec![0u16; a.area.height as usize];

    for y in 0..a.area.height {
        let row_start = y as usize * a.area.width as usize;
        let row_end = row_start + a.area.width as usize;
        let row = &next_buffer[row_start..row_end];
        let bg = row.last().map(|cell| cell.bg).unwrap_or(Color::Reset);

        let mut last_nonblank_column = 0usize;
        let mut column = 0usize;
        while column < row.len() {
            let cell = &row[column];
            let width = display_width(cell.symbol());
            if cell.symbol() != " " || cell.bg != bg || cell.modifier != Modifier::empty() {
                last_nonblank_column = column + width.saturating_sub(1);
            }
            column += width.max(1);
        }

        if last_nonblank_column + 1 < row.len() {
            let (x, y) = a.pos_of(row_start + last_nonblank_column + 1);
            updates.push(DrawCommand::ClearToEnd { x, y, bg });
        }
        last_nonblank_columns[y as usize] = last_nonblank_column as u16;
    }

    let mut invalidated: usize = 0;
    let mut to_skip: usize = 0;
    for (i, (current, previous)) in next_buffer.iter().zip(previous_buffer.iter()).enumerate() {
        if !current.skip && (current != previous || invalidated > 0) && to_skip == 0 {
            let (x, y) = a.pos_of(i);
            let row = i / a.area.width as usize;
            if x <= last_nonblank_columns[row] {
                updates.push(DrawCommand::Put {
                    x,
                    y,
                    cell: next_buffer[i].clone(),
                });
            }
        }
        to_skip = display_width(current.symbol()).saturating_sub(1);
        let affected_width = std::cmp::max(
            display_width(current.symbol()),
            display_width(previous.symbol()),
        );
        invalidated = std::cmp::max(affected_width, invalidated).saturating_sub(1);
    }
    updates
}

fn draw<I>(writer: &mut impl Write, commands: I) -> io::Result<()>
where
    I: Iterator<Item = DrawCommand>,
{
    let mut fg = Color::Reset;
    let mut bg = Color::Reset;
    let mut modifier = Modifier::empty();
    let mut last_pos: Option<Position> = None;

    for command in commands {
        let (x, y) = match command {
            DrawCommand::Put { x, y, .. } => (x, y),
            DrawCommand::ClearToEnd { x, y, .. } => (x, y),
        };
        if !matches!(last_pos, Some(p) if x == p.x + 1 && y == p.y) {
            queue!(writer, MoveTo(x, y))?;
        }
        last_pos = Some(Position { x, y });
        match command {
            DrawCommand::Put { cell, .. } => {
                if cell.modifier != modifier {
                    let diff = ModifierDiff {
                        from: modifier,
                        to: cell.modifier,
                    };
                    diff.queue(writer)?;
                    modifier = cell.modifier;
                }
                if cell.fg != fg || cell.bg != bg {
                    queue!(writer, SetColors(Colors::new(cell.fg.into(), cell.bg.into())))?;
                    fg = cell.fg;
                    bg = cell.bg;
                }
                queue!(writer, Print(cell.symbol()))?;
            }
            DrawCommand::ClearToEnd { bg: clear_bg, .. } => {
                queue!(writer, SetAttribute(crossterm::style::Attribute::Reset))?;
                modifier = Modifier::empty();
                queue!(writer, SetBackgroundColor(clear_bg.into()))?;
                bg = clear_bg;
                queue!(writer, Clear(crossterm::terminal::ClearType::UntilNewLine))?;
            }
        }
    }

    queue!(
        writer,
        SetForegroundColor(crossterm::style::Color::Reset),
        SetBackgroundColor(crossterm::style::Color::Reset),
        SetAttribute(crossterm::style::Attribute::Reset),
    )?;
    Ok(())
}

struct ModifierDiff {
    from: Modifier,
    to: Modifier,
}

impl ModifierDiff {
    fn queue<W: io::Write>(self, w: &mut W) -> io::Result<()> {
        use crossterm::style::Attribute as A;
        let removed = self.from - self.to;
        if removed.contains(Modifier::REVERSED) {
            queue!(w, SetAttribute(A::NoReverse))?;
        }
        if removed.contains(Modifier::BOLD) {
            queue!(w, SetAttribute(A::NormalIntensity))?;
            if self.to.contains(Modifier::DIM) {
                queue!(w, SetAttribute(A::Dim))?;
            }
        }
        if removed.contains(Modifier::ITALIC) {
            queue!(w, SetAttribute(A::NoItalic))?;
        }
        if removed.contains(Modifier::UNDERLINED) {
            queue!(w, SetAttribute(A::NoUnderline))?;
        }
        if removed.contains(Modifier::DIM) {
            queue!(w, SetAttribute(A::NormalIntensity))?;
        }
        if removed.contains(Modifier::CROSSED_OUT) {
            queue!(w, SetAttribute(A::NotCrossedOut))?;
        }
        if removed.contains(Modifier::SLOW_BLINK) || removed.contains(Modifier::RAPID_BLINK) {
            queue!(w, SetAttribute(A::NoBlink))?;
        }

        let added = self.to - self.from;
        if added.contains(Modifier::REVERSED) {
            queue!(w, SetAttribute(A::Reverse))?;
        }
        if added.contains(Modifier::BOLD) {
            queue!(w, SetAttribute(A::Bold))?;
        }
        if added.contains(Modifier::ITALIC) {
            queue!(w, SetAttribute(A::Italic))?;
        }
        if added.contains(Modifier::UNDERLINED) {
            queue!(w, SetAttribute(A::Underlined))?;
        }
        if added.contains(Modifier::DIM) {
            queue!(w, SetAttribute(A::Dim))?;
        }
        if added.contains(Modifier::CROSSED_OUT) {
            queue!(w, SetAttribute(A::CrossedOut))?;
        }
        if added.contains(Modifier::SLOW_BLINK) {
            queue!(w, SetAttribute(A::SlowBlink))?;
        }
        if added.contains(Modifier::RAPID_BLINK) {
            queue!(w, SetAttribute(A::RapidBlink))?;
        }
        Ok(())
    }
}
