pub mod custom_terminal;
pub mod insert_history;
pub mod markdown;

use openproof_core::{AppState, Overlay};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

/// Strip structured markers from assistant output for clean display.
/// Removes TITLE:, PHASE:, STATUS:, SEARCH:, FORMAL_TARGET:, ACCEPTED_TARGET:,
/// ASSUMPTION:, PAPER:, PAPER_NOTE:, NEXT:, THEOREM:, LEMMA:, LEMMA_CANDIDATE:
/// lines and ```latex fenced blocks. Keeps ```lean blocks and prose.
fn strip_markers(content: &str) -> String {
    let mut result = Vec::new();
    let mut in_latex_block = false;
    let mut in_lean_block = false;

    for line in content.lines() {
        let trimmed = line.trim();

        // Track fenced blocks
        if trimmed.starts_with("```latex") {
            in_latex_block = true;
            continue;
        }
        if trimmed.starts_with("```lean") {
            in_lean_block = true;
            result.push(line.to_string());
            continue;
        }
        if trimmed == "```" {
            if in_latex_block {
                in_latex_block = false;
                continue;
            }
            if in_lean_block {
                in_lean_block = false;
                result.push(line.to_string());
                continue;
            }
            result.push(line.to_string());
            continue;
        }

        // Skip latex block content
        if in_latex_block {
            continue;
        }

        // Keep lean block content
        if in_lean_block {
            result.push(line.to_string());
            continue;
        }

        // Skip structured marker lines
        let upper = trimmed.to_uppercase();
        if upper.starts_with("TITLE:")
            || upper.starts_with("PHASE:")
            || upper.starts_with("STATUS:")
            || upper.starts_with("SEARCH:")
            || upper.starts_with("FORMAL_TARGET:")
            || upper.starts_with("ACCEPTED_TARGET:")
            || upper.starts_with("ASSUMPTION:")
            || upper.starts_with("PAPER:")
            || upper.starts_with("PAPER_NOTE:")
            || upper.starts_with("PAPER_TEX:")
            || upper.starts_with("NEXT:")
            || upper.starts_with("THEOREM:")
            || upper.starts_with("LEMMA:")
            || upper.starts_with("LEMMA_CANDIDATE:")
            || upper.starts_with("OPTION:")
            || upper.starts_with("OPTION_TARGET:")
            || upper.starts_with("RECOMMENDED_OPTION:")
            || upper.starts_with("QUESTION:")
            || upper.starts_with("PROBLEM:")
        {
            continue;
        }

        result.push(line.to_string());
    }

    // Collapse multiple blank lines
    let mut cleaned = Vec::new();
    let mut prev_blank = false;
    for line in &result {
        if line.trim().is_empty() {
            if !prev_blank {
                cleaned.push(line.clone());
            }
            prev_blank = true;
        } else {
            cleaned.push(line.clone());
            prev_blank = false;
        }
    }

    cleaned.join("\n")
}

/// Render a single transcript entry into styled Lines (for flushing to scrollback).
pub fn render_entry(entry: &openproof_protocol::TranscriptEntry) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = vec![Line::from("")];
    match entry.role {
        openproof_protocol::MessageRole::User => {
            for content_line in entry.content.lines() {
                lines.push(Line::from(vec![
                    Span::styled("> ".to_string(), Style::default().fg(Color::DarkGray)),
                    Span::raw(content_line.to_string()),
                ]));
            }
        }
        openproof_protocol::MessageRole::Assistant => {
            let cleaned = strip_markers(&entry.content);
            lines.extend(markdown::render_markdown(&cleaned, Style::default()));
        }
        openproof_protocol::MessageRole::ToolCall => {
            let tool_name = entry.title.as_deref().unwrap_or("tool");
            // Summarize arguments (show first ~100 chars).
            let args_summary = if entry.content.chars().count() > 100 {
                format!("{}...", entry.content.chars().take(100).collect::<String>())
            } else {
                entry.content.clone()
            };
            lines.push(Line::from(vec![
                Span::styled(">> ", Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM)),
                Span::styled(tool_name.to_string(), Style::default().fg(Color::Cyan)),
                Span::styled(
                    format!("({args_summary})"),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }
        openproof_protocol::MessageRole::ToolResult => {
            let tool_name = entry.title.as_deref().unwrap_or("tool");
            // Show first ~10 lines of output.
            let output_lines: Vec<&str> = entry.content.lines().take(10).collect();
            let truncated = output_lines.len() < entry.content.lines().count();
            lines.push(Line::from(vec![
                Span::styled("<< ", Style::default().fg(Color::Green).add_modifier(Modifier::DIM)),
                Span::styled(
                    format!("{tool_name}: "),
                    Style::default().fg(Color::Green).add_modifier(Modifier::DIM),
                ),
            ]));
            for ol in &output_lines {
                // Color diff lines in tool output (green for +, red for -)
                let trimmed = ol.trim();
                let style = if trimmed.starts_with('+') && !trimmed.starts_with("+++") {
                    Style::default().fg(Color::Green)
                } else if trimmed.starts_with('-') && !trimmed.starts_with("---") {
                    Style::default().fg(Color::Red)
                } else if trimmed.starts_with("@@") {
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                lines.push(Line::from(Span::styled(format!("   {ol}"), style)));
            }
            if truncated {
                lines.push(Line::from(Span::styled(
                    "   ... (output truncated)".to_string(),
                    Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
                )));
            }
        }
        openproof_protocol::MessageRole::Diff => {
            let filename = entry.title.as_deref().unwrap_or("file");
            lines.push(Line::from(Span::styled(
                format!("  {filename}"),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )));
            for diff_line in entry.content.lines().take(30) {
                let style = if diff_line.starts_with('+') && !diff_line.starts_with("+++") {
                    Style::default().fg(Color::Green)
                } else if diff_line.starts_with('-') && !diff_line.starts_with("---") {
                    Style::default().fg(Color::Red)
                } else if diff_line.starts_with("@@") {
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                lines.push(Line::from(Span::styled(format!("  {diff_line}"), style)));
            }
        }
        openproof_protocol::MessageRole::Thought => {
            for thought_line in entry.content.lines() {
                lines.push(Line::from(Span::styled(
                    format!("  > {thought_line}"),
                    Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
                )));
            }
        }
        _ => {
            for content_line in entry.content.lines() {
                lines.push(Line::from(Span::styled(
                    content_line.to_string(),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }
    }
    lines
}

/// Draw the TUI frame (only unflushed content).
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
    // Force-clear input + status areas so the diff always repaints them.
    // This prevents keystrokes from "leaking" into the chat area during
    // rapid tool call updates.
    frame.render_widget(Clear, chunks[1]);
    frame.render_widget(Clear, chunks[2]);
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

fn splash_lines(area_height: u16) -> Vec<Line<'static>> {
    const ART: &[&str] = &[
        r"  ___                   ____                    __ ",
        r" / _ \ _ __   ___ _ __ |  _ \ _ __ ___   ___  / _|",
        r"| | | | '_ \ / _ \ '_ \| |_) | '__/ _ \ / _ \| |_ ",
        r"| |_| | |_) |  __/ | | |  __/| | | (_) | (_) |  _|",
        r" \___/| .__/ \___|_| |_|_|   |_|  \___/ \___/|_|  ",
        r"      |_|                                          ",
    ];
    // art(6) + blank(1) + tagline(1) + blank(1) + hint(1) = 10
    let content_height = ART.len() + 4;
    let top_pad = if (area_height as usize) > content_height {
        ((area_height as usize) - content_height) / 2
    } else {
        0
    };

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(top_pad + content_height);
    for _ in 0..top_pad {
        lines.push(Line::from(""));
    }

    let art_style = Style::default().fg(Color::Cyan);
    for art_line in ART {
        lines.push(Line::from(Span::styled(art_line.to_string(), art_style)).centered());
    }

    lines.push(Line::from(""));
    lines.push(
        Line::from(Span::styled(
            "Formal math proofs, conversationally".to_string(),
            Style::default().fg(Color::Gray),
        ))
        .centered(),
    );
    lines.push(Line::from(""));
    lines.push(
        Line::from(Span::styled(
            "Type a math problem or /help for commands.".to_string(),
            Style::default().fg(Color::DarkGray),
        ))
        .centered(),
    );

    lines
}

fn draw_chat_area(f: &mut custom_terminal::Frame<'_>, state: &mut AppState, area: Rect) {
    // Splash banner when chat is empty and no work in flight.
    let is_empty = state
        .current_session()
        .map(|s| s.transcript.is_empty())
        .unwrap_or(true);
    if is_empty && !state.turn_in_flight && state.streaming_text.is_empty() {
        let lines = splash_lines(area.height);
        let para = Paragraph::new(lines);
        f.render_widget(para, area);
        return;
    }

    let transcript_lines = state
        .current_session()
        .map(|session| {
            if session.transcript.is_empty() {
                vec![]
            } else {
                // Only render entries not yet flushed to scrollback.
                session
                    .transcript
                    .iter()
                    .skip(state.flushed_turn_count)
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
                                let cleaned = strip_markers(&entry.content);
                                lines.extend(markdown::render_markdown(
                                    &cleaned,
                                    Style::default(),
                                ));
                            }
                            openproof_protocol::MessageRole::ToolCall => {
                                let tool_name = entry.title.as_deref().unwrap_or("tool");
                                let args_summary = if entry.content.len() > 100 {
                                    format!("{}...", entry.content.chars().take(100).collect::<String>())
                                } else {
                                    entry.content.clone()
                                };
                                lines.push(Line::from(vec![
                                    Span::styled(">> ", Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM)),
                                    Span::styled(tool_name.to_string(), Style::default().fg(Color::Cyan)),
                                    Span::styled(
                                        format!("({args_summary})"),
                                        Style::default().fg(Color::DarkGray),
                                    ),
                                ]));
                            }
                            openproof_protocol::MessageRole::ToolResult => {
                                let tool_name = entry.title.as_deref().unwrap_or("tool");
                                let output_lines: Vec<&str> = entry.content.lines().take(10).collect();
                                let truncated = output_lines.len() < entry.content.lines().count();
                                lines.push(Line::from(vec![
                                    Span::styled("<< ", Style::default().fg(Color::Green).add_modifier(Modifier::DIM)),
                                    Span::styled(
                                        format!("{tool_name}: "),
                                        Style::default().fg(Color::Green).add_modifier(Modifier::DIM),
                                    ),
                                ]));
                                for ol in &output_lines {
                                    let trimmed = ol.trim();
                                    let style = if trimmed.starts_with('+') && !trimmed.starts_with("+++") {
                                        Style::default().fg(Color::Green)
                                    } else if trimmed.starts_with('-') && !trimmed.starts_with("---") {
                                        Style::default().fg(Color::Red)
                                    } else if trimmed.starts_with("@@") {
                                        Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM)
                                    } else {
                                        Style::default().fg(Color::DarkGray)
                                    };
                                    lines.push(Line::from(Span::styled(format!("   {ol}"), style)));
                                }
                                if truncated {
                                    lines.push(Line::from(Span::styled(
                                        "   ... (output truncated)".to_string(),
                                        Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
                                    )));
                                }
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

    // Streaming response and/or spinner when waiting for assistant
    let mut all_lines = transcript_lines;
    if state.turn_in_flight {
        if !state.streaming_text.is_empty() {
            // Show streamed text as it arrives
            all_lines.push(Line::from(""));
            all_lines.push(Line::from(Span::styled(
                "Assistant:".to_string(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )));
            if matches!(
                state.current_session().and_then(|s| s.transcript.last()).map(|e| e.role),
                Some(openproof_protocol::MessageRole::Assistant)
            ) {
                // Already have an assistant header from prior streaming
            }
            for line in state.streaming_text.lines() {
                all_lines.push(Line::from(format!("  {line}")));
            }
            // Cursor blink at end
            all_lines.push(Line::from(Span::styled(
                "  _".to_string(),
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            // No streaming text yet -- show spinner
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
                Span::styled("Thinking... ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("({elapsed_str})"),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }
    } else if state.verification_in_flight {
        let elapsed_ms = state
            .turn_started_at
            .map(|t| t.elapsed().as_millis())
            .unwrap_or(0);
        let elapsed_str = format!("{}s", elapsed_ms / 1000);
        all_lines.push(Line::from(vec![
            Span::styled("  > ", Style::default().fg(Color::Green)),
            Span::styled(
                format!("Verifying with Lean... ({elapsed_str})"),
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
    let is_autonomous = state.current_session()
        .map(|s| s.proof.is_autonomous_running)
        .unwrap_or(false);
    let auto_iter = state.current_session()
        .map(|s| s.proof.autonomous_iteration_count)
        .unwrap_or(0);

    let text = if state.turn_in_flight || state.verification_in_flight {
        let elapsed = state.activity_started_at
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0);
        let elapsed_str = if elapsed > 0 { format!(" ({elapsed}s)") } else { String::new() };
        let label = if !state.activity_label.is_empty() {
            state.activity_label.clone()
        } else if state.verification_in_flight {
            "verifying...".to_string()
        } else {
            "working...".to_string()
        };
        let iter_info = if state.tool_loop_active {
            format!(" (iter {}/40)", state.tool_loop_iteration + 1)
        } else {
            String::new()
        };
        let auto_prefix = if is_autonomous {
            format!(" autonomous (iter {auto_iter}) | ")
        } else {
            " ".to_string()
        };
        let activity = format!("{auto_prefix}{label}{elapsed_str}{iter_info}");
        Span::styled(activity, Style::default().fg(Color::Yellow))
    } else if is_autonomous {
        let full = state.current_session()
            .map(|s| s.proof.full_autonomous)
            .unwrap_or(false);
        let mode_label = if full { "full autonomous" } else { "autonomous" };
        Span::styled(
            format!(" {mode_label} (iter {auto_iter}) | idle between steps (shift+tab to cycle)"),
            Style::default().fg(Color::Cyan),
        )
    } else {
        Span::styled(
            " (shift+tab to cycle mode)".to_string(),
            Style::default().fg(Color::DarkGray),
        )
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
