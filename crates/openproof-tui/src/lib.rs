use openproof_core::AppState;
use openproof_protocol::ProofNodeStatus;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};

/// Draw a chat-centric TUI: scrolling transcript + input area + status line.
/// No side panels. Proof state is available via /proof and /status commands.
pub fn draw(frame: &mut Frame<'_>, state: &AppState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // status bar
            Constraint::Min(6),   // transcript
            Constraint::Length(3), // composer input
        ])
        .split(frame.area());

    let status_area = chunks[0];
    let transcript_area = chunks[1];
    let composer_area = chunks[2];

    // --- Status bar (single line) ---
    let session_info = state.current_session().map(|s| {
        let phase = &s.proof.phase;
        let node_count = s.proof.nodes.len();
        let verified = s.proof.nodes.iter()
            .filter(|n| n.status == ProofNodeStatus::Verified)
            .count();
        let focus = s.proof.active_node_id.as_deref()
            .and_then(|id| s.proof.nodes.iter().find(|n| n.id == id))
            .map(|n| n.label.clone())
            .unwrap_or_else(|| "none".to_string());
        (s.title.clone(), phase.clone(), node_count, verified, focus)
    });

    let status_line = if let Some((title, phase, nodes, verified, focus)) = session_info {
        let turn = if state.turn_in_flight { "turn" } else { "" };
        let verify = if state.verification_in_flight { "verify" } else { "" };
        let activity: Vec<&str> = [turn, verify].into_iter().filter(|s| !s.is_empty()).collect();
        let activity_str = if activity.is_empty() {
            "idle".to_string()
        } else {
            activity.join("+")
        };
        vec![
            Span::styled(" openproof ", Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::raw(format!(" {title}")),
            Span::styled(format!(" | {phase}"), Style::default().fg(Color::DarkGray)),
            Span::styled(format!(" | {nodes} nodes, {verified} verified"), Style::default().fg(Color::DarkGray)),
            Span::styled(format!(" | focus: {focus}"), Style::default().fg(Color::DarkGray)),
            Span::styled(format!(" | {activity_str}"), Style::default().fg(if state.turn_in_flight || state.verification_in_flight { Color::Yellow } else { Color::DarkGray })),
        ]
    } else {
        vec![
            Span::styled(" openproof ", Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::raw(" no session"),
        ]
    };
    frame.render_widget(
        Paragraph::new(Line::from(status_line)).style(Style::default()),
        status_area,
    );

    // --- Transcript (scrolling chat) ---
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
                            openproof_protocol::MessageRole::User => ("you", Color::Green),
                            openproof_protocol::MessageRole::Assistant => ("assistant", Color::Blue),
                            openproof_protocol::MessageRole::System => ("system", Color::Magenta),
                            openproof_protocol::MessageRole::Notice => ("notice", Color::Yellow),
                        };
                        let title = entry.title.clone().unwrap_or_default();
                        let header = if title.is_empty() {
                            role_label.to_string()
                        } else {
                            format!("{role_label}: {title}")
                        };

                        let mut lines = vec![
                            Line::from(""),
                            Line::from(Span::styled(
                                header,
                                Style::default().fg(role_color).add_modifier(Modifier::BOLD),
                            )),
                        ];
                        for content_line in entry.content.lines() {
                            lines.push(Line::from(content_line.to_string()));
                        }
                        lines
                    })
                    .collect::<Vec<_>>()
            }
        })
        .unwrap_or_else(|| vec![Line::from("No active session.")]);

    let transcript = Paragraph::new(Text::from(transcript_lines))
        .wrap(Wrap { trim: false })
        .scroll((state.transcript_scroll, 0));
    frame.render_widget(transcript, transcript_area);

    // --- Composer (input area) ---
    let prompt_char = if state.turn_in_flight { "..." } else { ">" };
    let composer_block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Span::styled(
            format!(" {prompt_char} "),
            Style::default().fg(Color::Cyan),
        ));
    let composer = Paragraph::new(state.composer.as_str())
        .block(composer_block)
        .wrap(Wrap { trim: false });
    frame.render_widget(composer, composer_area);

    // --- Question modal (if active) ---
    if state.has_open_question() {
        render_question_modal(frame, state);
    }
}

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
