pub mod custom_terminal;
pub mod markdown;

use openproof_core::{AppState, Overlay};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

/// Draw using the custom inline-viewport Frame.
pub fn draw(frame: &mut custom_terminal::Frame<'_>, state: &mut AppState) {
    let area = frame.area();

    let prefix_len = 2; // "> "
    let input_height = compute_input_height(&state.composer, prefix_len, area.width);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),               // chat area
            Constraint::Length(input_height), // input
            Constraint::Length(1),            // status bar
        ])
        .split(area);

    draw_chat_area(frame, state, chunks[0]);
    draw_input_area(frame, state, chunks[1]);

    if state.command_mode {
        draw_command_bar(frame, state, chunks[2]);
        if !state.command_completions.is_empty() {
            draw_completion_popup(frame, state, chunks[2]);
        }
    } else {
        draw_status_bar(frame, state, chunks[2]);
    }

    if state.has_open_question() {
        render_question_modal(frame, state);
    }

    // Overlays render last (on top of everything).
    if let Some(ref overlay) = state.overlay {
        draw_overlay(frame, state, overlay, area);
    }
}

// ---------------------------------------------------------------------------
// Chat area (scrollable transcript)
// ---------------------------------------------------------------------------

fn draw_chat_area(f: &mut custom_terminal::Frame<'_>, state: &mut AppState, area: Rect) {
    let transcript_lines = state
        .current_session()
        .map(|session| {
            if session.transcript.is_empty() {
                vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        "  Type a math problem or /help for commands.",
                        Style::default().fg(Color::DarkGray),
                    )),
                ]
            } else {
                session
                    .transcript
                    .iter()
                    .flat_map(|entry| {
                        let mut lines: Vec<Line<'static>> = vec![Line::from("")];

                        match entry.role {
                            openproof_protocol::MessageRole::User => {
                                for content_line in entry.content.lines() {
                                    lines.push(Line::from(vec![
                                        Span::styled(
                                            "> ".to_string(),
                                            Style::default().fg(Color::DarkGray),
                                        ),
                                        Span::raw(content_line.to_string()),
                                    ]));
                                }
                            }
                            openproof_protocol::MessageRole::Assistant => {
                                lines.extend(markdown::render_markdown(
                                    &entry.content,
                                    Style::default(),
                                ));
                            }
                            _ => {
                                // System/Notice: dim text, no label
                                for content_line in entry.content.lines() {
                                    lines.push(Line::from(Span::styled(
                                        content_line.to_string(),
                                        Style::default().fg(Color::DarkGray),
                                    )));
                                }
                            }
                        }
                        lines
                    })
                    .collect::<Vec<_>>()
            }
        })
        .unwrap_or_else(|| vec![Line::from("No active session.")]);

    // Thinking spinner when waiting for assistant
    let mut all_lines = transcript_lines;
    if state.turn_in_flight {
        let elapsed_ms = state
            .turn_started_at
            .map(|t| t.elapsed().as_millis())
            .unwrap_or(0);
        const FRAMES: &[&str] = &["-", "\\", "|", "/"];
        let spinner = FRAMES[(elapsed_ms / 200) as usize % FRAMES.len()];
        let elapsed_str = if elapsed_ms < 60_000 {
            format!("{}s", elapsed_ms / 1000)
        } else {
            format!(
                "{}m {}s",
                elapsed_ms / 60_000,
                (elapsed_ms % 60_000) / 1000
            )
        };
        all_lines.push(Line::from(vec![
            Span::styled(
                format!("  {spinner} "),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled("Working... ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("({elapsed_str})"),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }

    let text = Text::from(all_lines);
    let total_visual = Paragraph::new(text.clone())
        .wrap(Wrap { trim: false })
        .line_count(area.width);
    state.total_visual_lines = total_visual;
    state.visible_height = area.height as usize;

    let max_scroll = total_visual.saturating_sub(state.visible_height);
    if state.scroll_offset > max_scroll {
        state.scroll_offset = max_scroll;
    }

    let scroll_from_top = if total_visual > state.visible_height {
        max_scroll
            .saturating_sub(state.scroll_offset)
            .min(u16::MAX as usize)
    } else {
        0
    };

    let para = Paragraph::new(text)
        .scroll((scroll_from_top as u16, 0))
        .wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

// ---------------------------------------------------------------------------
// Smooth scrollbar (half-block rendering for 2x vertical resolution)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Composer (input area)
// ---------------------------------------------------------------------------

fn draw_input_area(f: &mut custom_terminal::Frame<'_>, state: &AppState, area: Rect) {
    let mut spans = vec![Span::styled(
        "> ".to_string(),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )];

    spans.extend(build_cursor_spans(&state.composer, state.composer_cursor));

    let prefix_len = 2;
    let usable_width = area.width as usize;
    let visible_rows = area.height.saturating_sub(1) as usize;
    let cursor_row = cursor_visual_row(&state.composer, state.composer_cursor, prefix_len, usable_width);
    let scroll_offset = if visible_rows > 0 && cursor_row >= visible_rows {
        (cursor_row - visible_rows + 1) as u16
    } else {
        0
    };

    let input_para = Paragraph::new(Line::from(spans))
        .wrap(Wrap { trim: false })
        .scroll((scroll_offset, 0))
        .block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(Color::Rgb(60, 60, 60))),
        );
    f.render_widget(input_para, area);
}

// ---------------------------------------------------------------------------
// Status bar
// ---------------------------------------------------------------------------

fn draw_status_bar(f: &mut custom_terminal::Frame<'_>, state: &AppState, area: Rect) {
    let text = if state.turn_in_flight || state.verification_in_flight {
        let activity = match (state.turn_in_flight, state.verification_in_flight) {
            (true, true) => "working + verifying...",
            (true, false) => "working...",
            (false, true) => "verifying...",
            _ => "",
        };
        Span::styled(
            format!(" {activity}"),
            Style::default().fg(Color::Yellow),
        )
    } else {
        Span::styled(" ".to_string(), Style::default().fg(Color::DarkGray))
    };
    let para =
        Paragraph::new(Line::from(text)).style(Style::default().bg(Color::Rgb(30, 30, 30)));
    f.render_widget(para, area);
}

// ---------------------------------------------------------------------------
// Command bar (/ mode)
// ---------------------------------------------------------------------------

fn draw_command_bar(f: &mut custom_terminal::Frame<'_>, state: &AppState, area: Rect) {
    let mut spans = vec![Span::styled(
        "/".to_string(),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )];
    spans.extend(build_cursor_spans(
        &state.command_buffer,
        state.command_cursor,
    ));
    let para = Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::Rgb(30, 30, 30)));
    f.render_widget(para, area);
}

fn draw_completion_popup(f: &mut custom_terminal::Frame<'_>, state: &AppState, cmd_area: Rect) {
    let completions = &state.command_completions;
    let max_show = completions.len().min(8);
    if max_show == 0 {
        return;
    }

    let popup_height = max_show as u16;
    let popup_y = cmd_area.y.saturating_sub(popup_height);
    let popup_width = completions
        .iter()
        .map(|c| c.len())
        .max()
        .unwrap_or(10)
        .min(40) as u16
        + 4;

    let popup_area = Rect {
        x: cmd_area.x + 1,
        y: popup_y,
        width: popup_width.min(cmd_area.width),
        height: popup_height,
    };

    f.render_widget(Clear, popup_area);

    let lines: Vec<Line<'static>> = completions
        .iter()
        .take(max_show)
        .enumerate()
        .map(|(i, c)| {
            let style = if state.completion_idx == Some(i) {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default().fg(Color::White)
            };
            Line::from(Span::styled(format!(" /{c} "), style))
        })
        .collect();

    let para = Paragraph::new(lines).style(Style::default().bg(Color::Rgb(40, 40, 40)));
    f.render_widget(para, popup_area);
}

// ---------------------------------------------------------------------------
// Overlays
// ---------------------------------------------------------------------------

fn draw_overlay(f: &mut custom_terminal::Frame<'_>, state: &AppState, overlay: &Overlay, area: Rect) {
    match overlay {
        Overlay::SessionPicker { selected } => draw_session_picker(f, state, *selected, area),
        Overlay::FocusPicker { items, selected } => {
            draw_focus_picker(f, items, *selected, area)
        }
    }
}

fn draw_session_picker(f: &mut custom_terminal::Frame<'_>, state: &AppState, selected: usize, area: Rect) {
    let popup = centered_rect(75, 60, area);
    f.render_widget(Clear, popup);

    let block = Block::default()
        .title(" Sessions (Enter=resume, Esc=cancel) ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .style(Style::default().bg(Color::Rgb(25, 25, 25)));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    if inner.height < 2 || state.sessions.is_empty() {
        let msg = Paragraph::new(Line::from(Span::styled(
            " No sessions",
            Style::default().fg(Color::DarkGray),
        )));
        f.render_widget(msg, inner);
        return;
    }

    // Header row
    let header_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: 1,
    };
    let header = Paragraph::new(Line::from(Span::styled(
        format!(" {:30}  {:>5}  {:>5}  {}", "Title", "Msgs", "Nodes", "Updated"),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )));
    f.render_widget(header, header_area);

    let list_area = Rect {
        x: inner.x,
        y: inner.y + 1,
        width: inner.width,
        height: inner.height.saturating_sub(1),
    };

    let visible = list_area.height as usize;
    let scroll_offset = if selected >= visible {
        selected - visible + 1
    } else {
        0
    };

    let lines: Vec<Line<'static>> = state
        .sessions
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(visible)
        .map(|(i, s)| {
            let style = if i == selected {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default().fg(Color::White)
            };
            let active = if i == state.selected_session { "*" } else { " " };
            let ts: String = s.updated_at.chars().take(10).collect();
            let title: String = s.title.chars().take(28).collect();
            let text = format!(
                "{active}{:30}  {:>5}  {:>5}  {}",
                title,
                s.transcript.len(),
                s.proof.nodes.len(),
                ts,
            );
            let truncated: String = text.chars().take(inner.width as usize).collect();
            Line::from(Span::styled(truncated, style))
        })
        .collect();

    f.render_widget(Paragraph::new(lines), list_area);
}

fn draw_focus_picker(
    f: &mut custom_terminal::Frame<'_>,
    items: &[(String, String, String)],
    selected: usize,
    area: Rect,
) {
    let popup = centered_rect(70, 50, area);
    f.render_widget(Clear, popup);

    let block = Block::default()
        .title(" Focus target (Enter=select, Esc=cancel) ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .style(Style::default().bg(Color::Rgb(25, 25, 25)));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    if inner.height < 2 || items.is_empty() {
        let msg = Paragraph::new(Line::from(Span::styled(
            " No focusable targets",
            Style::default().fg(Color::DarkGray),
        )));
        f.render_widget(msg, inner);
        return;
    }

    let header_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: 1,
    };
    let header = Paragraph::new(Line::from(Span::styled(
        format!(" {:12}  {:30}  {}", "Kind", "Label", "ID"),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )));
    f.render_widget(header, header_area);

    let list_area = Rect {
        x: inner.x,
        y: inner.y + 1,
        width: inner.width,
        height: inner.height.saturating_sub(1),
    };

    let visible = list_area.height as usize;
    let scroll_offset = if selected >= visible {
        selected - visible + 1
    } else {
        0
    };

    let lines: Vec<Line<'static>> = items
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(visible)
        .map(|(i, (id, label, kind))| {
            let style = if i == selected {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default().fg(Color::White)
            };
            let label_trunc: String = label.chars().take(28).collect();
            let kind_trunc: String = kind.chars().take(12).collect();
            let id_trunc: String = id.chars().take(16).collect();
            let text = format!(" {:12}  {:30}  {}", kind_trunc, label_trunc, id_trunc);
            let truncated: String = text.chars().take(inner.width as usize).collect();
            Line::from(Span::styled(truncated, style))
        })
        .collect();

    f.render_widget(Paragraph::new(lines), list_area);
}

// ---------------------------------------------------------------------------
// Cursor rendering
// ---------------------------------------------------------------------------

/// Build spans for text with a visible block cursor at the given byte position.
fn build_cursor_spans(text: &str, cursor: usize) -> Vec<Span<'static>> {
    let mut spans = Vec::new();

    if text.is_empty() {
        spans.push(Span::styled(
            " ".to_string(),
            Style::default().fg(Color::Black).bg(Color::White),
        ));
        return spans;
    }

    let (before, rest) = text.split_at(cursor.min(text.len()));

    if !before.is_empty() {
        spans.push(Span::raw(before.to_string()));
    }

    if rest.is_empty() {
        spans.push(Span::styled(
            " ".to_string(),
            Style::default().fg(Color::Black).bg(Color::White),
        ));
    } else {
        let mut chars = rest.chars();
        let cursor_char = chars.next().unwrap();
        spans.push(Span::styled(
            cursor_char.to_string(),
            Style::default().fg(Color::Black).bg(Color::White),
        ));
        let after: String = chars.collect();
        if !after.is_empty() {
            spans.push(Span::raw(after));
        }
    }

    spans
}

/// Compute the height needed for the input area, accounting for text wrapping.
fn compute_input_height(text: &str, prefix_len: usize, area_width: u16) -> u16 {
    let usable = area_width as usize;
    if usable == 0 {
        return 3;
    }
    let char_count = prefix_len + text.chars().count();
    let visual_lines = if char_count == 0 {
        1
    } else {
        char_count.div_ceil(usable)
    };
    (visual_lines as u16 + 2).clamp(3, 10)
}

/// Determine which visual row the cursor occupies when the input wraps.
fn cursor_visual_row(text: &str, cursor: usize, prefix_len: usize, width: usize) -> usize {
    if width == 0 {
        return 0;
    }
    let chars_before = text[..cursor.min(text.len())].chars().count();
    (prefix_len + chars_before) / width
}

// ---------------------------------------------------------------------------
// Question modal (unchanged domain logic)
// ---------------------------------------------------------------------------

fn render_question_modal(frame: &mut custom_terminal::Frame<'_>, state: &AppState) {
    let Some(question) = state.pending_question() else {
        return;
    };
    let area = centered_rect(78, 70, frame.area());
    frame.render_widget(Clear, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(7),
            Constraint::Length(4),
        ])
        .split(area);

    let header = Paragraph::new(Text::from(vec![
        Line::from(Span::styled(
            "Clarification Required",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(question.prompt.clone()),
        Line::from("Select an option with up/down and press Enter."),
    ]))
    .block(Block::default().borders(Borders::ALL).title("Question"))
    .wrap(Wrap { trim: false });
    frame.render_widget(header, chunks[0]);

    let items = question
        .options
        .iter()
        .map(|option| {
            let recommended = question
                .recommended_option_id
                .as_ref()
                .map(|value| value == &option.id)
                .unwrap_or(false);
            let mut lines = vec![Line::from(vec![
                Span::styled(
                    option.id.clone(),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(
                    option.label.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                if recommended {
                    Span::styled("  recommended", Style::default().fg(Color::Yellow))
                } else {
                    Span::raw("")
                },
            ])];
            if !option.summary.trim().is_empty() {
                lines.push(Line::from(Span::styled(
                    option.summary.clone(),
                    Style::default().fg(Color::Gray),
                )));
            }
            if !option.formal_target.trim().is_empty() {
                lines.push(Line::from(option.formal_target.clone()));
            }
            ListItem::new(lines)
        })
        .collect::<Vec<_>>();
    let mut list_state = ListState::default();
    list_state.select(Some(
        state
            .selected_question_option
            .min(question.options.len().saturating_sub(1)),
    ));
    let options = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Options"))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");
    frame.render_stateful_widget(options, chunks[1], &mut list_state);

    let mut footer_lines = vec![Line::from(format!("status: {}", question.status))];
    if let Some(answer) = &question.answer_text {
        footer_lines.push(Line::from(format!("latest answer: {}", answer)));
    }
    let footer = Paragraph::new(Text::from(footer_lines))
        .block(Block::default().borders(Borders::ALL).title("Resolution"))
        .wrap(Wrap { trim: false });
    frame.render_widget(footer, chunks[2]);
}

fn centered_rect(width_percent: u16, height_percent: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - height_percent) / 2),
            Constraint::Percentage(height_percent),
            Constraint::Percentage(100 - height_percent - ((100 - height_percent) / 2)),
        ])
        .split(area);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - width_percent) / 2),
            Constraint::Percentage(width_percent),
            Constraint::Percentage(100 - width_percent - ((100 - width_percent) / 2)),
        ])
        .split(vertical[1]);
    horizontal[1]
}
