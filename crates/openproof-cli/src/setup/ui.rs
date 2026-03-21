//! Rendering for the setup wizard.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use super::app::{SetupApp, Step, CORPUS_MODES, PROVIDERS};

pub fn draw(f: &mut Frame, app: &SetupApp) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // title
            Constraint::Min(10),  // content
            Constraint::Length(1), // footer
        ])
        .split(area);

    draw_title(f, chunks[0]);
    match app.step {
        Step::Provider => draw_provider_step(f, app, chunks[1]),
        Step::Corpus => draw_corpus_step(f, app, chunks[1]),
        Step::Finish => draw_finish(f, app, chunks[1]),
    }
    draw_footer(f, app, chunks[2]);
}

fn draw_title(f: &mut Frame, area: Rect) {
    let title = Paragraph::new(vec![
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(
                "openproof setup",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
    ]);
    f.render_widget(title, area);
}

fn draw_footer(f: &mut Frame, app: &SetupApp, area: Rect) {
    let step_num = match app.step {
        Step::Provider => 1,
        Step::Corpus => 2,
        Step::Finish => 3,
    };
    let hint = match app.step {
        Step::Finish => "Enter to start",
        _ => "Up/Down to select, Enter to confirm, Esc to go back",
    };
    let footer = Paragraph::new(Line::from(vec![
        Span::styled(
            format!("  Step {step_num}/2  "),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(hint, Style::default().fg(Color::DarkGray)),
    ]))
    .style(Style::default().bg(Color::Rgb(20, 20, 20)));
    f.render_widget(footer, area);
}

fn draw_provider_step(f: &mut Frame, app: &SetupApp, area: Rect) {
    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Model Provider",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "  Choose how openproof connects to an LLM.",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
    ];

    for (i, (_, label, _)) in PROVIDERS.iter().enumerate() {
        let selected = i == app.provider_selected;
        let marker = if selected { "> " } else { "  " };
        let style = if selected {
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        lines.push(Line::from(Span::styled(
            format!("  {marker}{label}"),
            style,
        )));
    }

    if app.entering_key {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  Enter API key:",
            Style::default().fg(Color::Yellow),
        )));
        let masked: String = "*".repeat(app.api_key_input.len());
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                if masked.is_empty() {
                    "(paste or type key)".to_string()
                } else {
                    masked
                },
                Style::default().fg(Color::White),
            ),
        ]));
    }

    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

fn draw_corpus_step(f: &mut Frame, app: &SetupApp, area: Rect) {
    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Corpus Mode",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "  The corpus stores verified Lean theorems for retrieval during proofs.",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
    ];

    for (i, (_, label)) in CORPUS_MODES.iter().enumerate() {
        let selected = i == app.corpus_selected;
        let marker = if selected { "> " } else { "  " };
        let style = if selected {
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        lines.push(Line::from(Span::styled(
            format!("  {marker}{label}"),
            style,
        )));
    }

    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

fn draw_finish(f: &mut Frame, app: &SetupApp, area: Rect) {
    let (provider_id, provider_label, _) = PROVIDERS[app.provider_selected];
    let (corpus_id, corpus_label) = CORPUS_MODES[app.corpus_selected];

    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Setup Complete",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Provider: ", Style::default().fg(Color::DarkGray)),
            Span::styled(provider_label, Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  Corpus:   ", Style::default().fg(Color::DarkGray)),
            Span::styled(corpus_label, Style::default().fg(Color::White)),
        ]),
        Line::from(""),
        if corpus_id == "local" {
            Line::from(Span::styled(
                "  Mathlib will be auto-ingested on first launch.",
                Style::default().fg(Color::DarkGray),
            ))
        } else {
            Line::from(Span::styled(
                "  Your verified proofs will contribute to the shared corpus.",
                Style::default().fg(Color::DarkGray),
            ))
        },
        Line::from(""),
        Line::from(Span::styled(
            "  Press Enter to start openproof.",
            Style::default().fg(Color::Cyan),
        )),
    ];

    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    f.render_widget(para, area);
}
