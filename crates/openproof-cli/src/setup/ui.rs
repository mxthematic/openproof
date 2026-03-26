//! Rendering for the setup wizard.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use super::app::{SetupApp, Step, CORPUS_MODES, PROVIDERS, PROVER_MODELS};

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
        Step::ProverModel => draw_prover_step(f, app, chunks[1]),
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
        Step::ProverModel => 3,
        Step::Finish => 4,
    };
    let hint = match app.step {
        Step::Finish => "Enter to start",
        _ => "Up/Down to select, Enter to confirm, Esc to go back",
    };
    let footer = Paragraph::new(Line::from(vec![
        Span::styled(
            format!("  Step {step_num}/3  "),
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
            "  Cloud mode connects to OpenProof servers for faster, more accurate proofs.",
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

fn draw_prover_step(f: &mut Frame, app: &SetupApp, area: Rect) {
    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Prover Model",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "  A local model dramatically improves tactic search. Requires ollama.",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            "  Install: brew install ollama && ollama serve",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
    ];

    for (i, (_, label, _)) in PROVER_MODELS.iter().enumerate() {
        let selected = i == app.prover_selected;
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
    let (_, provider_label, _) = PROVIDERS[app.provider_selected];
    let (corpus_id, corpus_label) = CORPUS_MODES[app.corpus_selected];
    let (prover_id, prover_label, _) = PROVER_MODELS[app.prover_selected];

    let mut lines = vec![
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
        Line::from(vec![
            Span::styled("  Prover:   ", Style::default().fg(Color::DarkGray)),
            Span::styled(prover_label, Style::default().fg(Color::White)),
        ]),
        Line::from(""),
    ];

    if prover_id != "none" {
        lines.push(Line::from(Span::styled(
            "  The model will be downloaded on first use via ollama.",
            Style::default().fg(Color::DarkGray),
        )));
    }

    if corpus_id == "local" {
        lines.push(Line::from(Span::styled(
            "  Mathlib will be auto-ingested on first launch.",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "  Connected to OpenProof cloud.",
            Style::default().fg(Color::DarkGray),
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Press Enter to start openproof.",
        Style::default().fg(Color::Cyan),
    )));

    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    f.render_widget(para, area);
}
