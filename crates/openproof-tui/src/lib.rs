pub mod markdown;

use openproof_core::AppState;
use openproof_protocol::ProofNodeStatus;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};

/// Draw the full TUI frame: header / chat area / input / status bar.
pub fn draw(frame: &mut Frame<'_>, state: &mut AppState) {
    let area = frame.area();

    let prefix_len = 2; // "> "
    let input_height = compute_input_height(&state.composer, prefix_len, area.width);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),            // header
            Constraint::Min(5),               // chat area
            Constraint::Length(input_height), // input
            Constraint::Length(1),            // status bar
        ])
        .split(area);

    draw_header(frame, state, chunks[0]);
    draw_chat_area(frame, state, chunks[1]);
    draw_input_area(frame, state, chunks[2]);
    draw_status_bar(frame, state, chunks[3]);

    if state.has_open_question() {
        render_question_modal(frame, state);
    }
}

// ---------------------------------------------------------------------------
// Header bar
// ---------------------------------------------------------------------------

fn draw_header(f: &mut Frame<'_>, state: &AppState, area: Rect) {
    let session_info = state.current_session().map(|s| {
        let phase = &s.proof.phase;
        let node_count = s.proof.nodes.len();
        let verified = s
            .proof
            .nodes
            .iter()
            .filter(|n| n.status == ProofNodeStatus::Verified)
            .count();
        let focus = s
            .proof
            .active_node_id
            .as_deref()
            .and_then(|id| s.proof.nodes.iter().find(|n| n.id == id))
            .map(|n| n.label.clone())
            .unwrap_or_else(|| "none".to_string());
        (s.title.clone(), phase.clone(), node_count, verified, focus)
    });

    let spans = if let Some((title, phase, nodes, verified, focus)) = session_info {
        let activity_str = match (state.turn_in_flight, state.verification_in_flight) {
            (true, true) => "turn+verify",
            (true, false) => "turn",
            (false, true) => "verify",
            (false, false) => "idle",
        };
        let activity_color = if state.turn_in_flight || state.verification_in_flight {
            Color::Yellow
        } else {
            Color::DarkGray
        };
        vec![
            Span::styled(
                " openproof ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(" {title}")),
            Span::styled(format!(" | {phase}"), Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!(" | {nodes} nodes, {verified} verified"),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                format!(" | focus: {focus}"),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                format!(" | {activity_str}"),
                Style::default().fg(activity_color),
            ),
        ]
    } else {
        vec![
            Span::styled(
                " openproof ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" no session"),
        ]
    };

    let para =
        Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::Rgb(30, 30, 30)));
    f.render_widget(para, area);
}

// ---------------------------------------------------------------------------
// Chat area (scrollable transcript)
// ---------------------------------------------------------------------------

fn draw_chat_area(f: &mut Frame<'_>, state: &mut AppState, area: Rect) {
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
                        let (role_label, role_color) = match entry.role {
                            openproof_protocol::MessageRole::User => ("You:", Color::Cyan),
                            openproof_protocol::MessageRole::Assistant => {
                                ("Assistant:", Color::White)
                            }
                            openproof_protocol::MessageRole::System => ("System:", Color::Magenta),
                            openproof_protocol::MessageRole::Notice => ("Notice:", Color::Yellow),
                        };

                        let mut lines = vec![
                            Line::from(""),
                            Line::from(Span::styled(
                                role_label.to_string(),
                                Style::default()
                                    .fg(role_color)
                                    .add_modifier(Modifier::BOLD),
                            )),
                        ];

                        // Use markdown rendering for assistant messages.
                        if matches!(entry.role, openproof_protocol::MessageRole::Assistant) {
                            lines.extend(markdown::render_markdown(
                                &entry.content,
                                Style::default(),
                            ));
                        } else if matches!(entry.role, openproof_protocol::MessageRole::User) {
                            for content_line in entry.content.lines() {
                                lines.push(Line::from(format!("  {content_line}")));
                            }
                        } else {
                            for content_line in entry.content.lines() {
                                lines.push(Line::from(content_line.to_string()));
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

    // Scroll indicator when scrolled up
    if state.scroll_offset > 0 {
        let pct = if max_scroll > 0 {
            100 * state.scroll_offset / max_scroll
        } else {
            100
        };
        let indicator = Line::from(Span::styled(
            format!(
                " Scroll: {}% ({}/{} lines) ",
                pct, state.scroll_offset, max_scroll
            ),
            Style::default()
                .fg(Color::Yellow)
                .bg(Color::Rgb(40, 40, 40)),
        ));
        let indicator_area = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        };
        f.render_widget(Paragraph::new(vec![indicator]), indicator_area);
    }
}

// ---------------------------------------------------------------------------
// Composer (input area)
// ---------------------------------------------------------------------------

fn draw_input_area(f: &mut Frame<'_>, state: &AppState, area: Rect) {
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

fn draw_status_bar(f: &mut Frame<'_>, state: &AppState, area: Rect) {
    let status_text = if state.status.is_empty() {
        "Ready.".to_string()
    } else {
        state.status.clone()
    };
    let para = Paragraph::new(Line::from(vec![
        Span::raw(" "),
        Span::styled(status_text, Style::default().fg(Color::DarkGray)),
    ]))
    .style(Style::default().bg(Color::Rgb(30, 30, 30)));
    f.render_widget(para, area);
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

fn render_question_modal(frame: &mut Frame<'_>, state: &AppState) {
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
