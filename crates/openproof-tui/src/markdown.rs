//! Lightweight markdown-to-ratatui converter.
//!
//! Renders a subset of markdown into styled `Line`/`Span` sequences for display
//! in a terminal TUI. No external dependencies beyond ratatui.
//!
//! Supported syntax:
//! - `**bold**` and `__bold__`
//! - `` `inline code` ``
//! - `# Headers` (levels 1-3)
//! - Fenced code blocks (` ``` `)
//! - `- ` / `* ` bullet lists
//! - `1. ` numbered lists
//! - `> ` blockquotes (simple dim treatment)
//! - `[text](url)` links
//!
//! Unsupported syntax (images, tables, nested lists) passes through as plain text.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Style for code block content: light gray on dark gray.
const CODE_BLOCK_STYLE: Style = Style::new()
    .fg(Color::Rgb(180, 180, 180))
    .bg(Color::Rgb(40, 40, 40));

/// Style for the code fence lines themselves (``` markers).
const CODE_FENCE_STYLE: Style = Style::new().fg(Color::Rgb(100, 100, 100));

/// Render a markdown text block into styled ratatui Lines.
pub fn render_markdown(text: &str, base_style: Style) -> Vec<Line<'static>> {
    let mut result = Vec::new();
    let mut in_code_block = false;

    for raw_line in text.split('\n') {
        if is_code_fence(raw_line) {
            if in_code_block {
                result.push(Line::from(Span::styled(
                    raw_line.to_string(),
                    CODE_FENCE_STYLE,
                )));
                in_code_block = false;
            } else {
                result.push(Line::from(Span::styled(
                    raw_line.to_string(),
                    CODE_FENCE_STYLE,
                )));
                in_code_block = true;
            }
            continue;
        }

        if in_code_block {
            result.push(Line::from(Span::styled(
                raw_line.to_string(),
                CODE_BLOCK_STYLE,
            )));
            continue;
        }

        if let Some((level, content)) = strip_header_prefix(raw_line) {
            let header_style = match level {
                1 => base_style.fg(Color::Cyan).add_modifier(Modifier::BOLD),
                _ => base_style.fg(Color::White).add_modifier(Modifier::BOLD),
            };
            let spans = style_inline(content, header_style);
            result.push(Line::from(spans));
        } else if let Some(content) = is_bullet(raw_line) {
            let mut spans = vec![Span::styled(
                "  - ".to_string(),
                base_style.fg(Color::DarkGray),
            )];
            spans.extend(style_inline(content, base_style));
            result.push(Line::from(spans));
        } else if let Some((num, content)) = is_numbered(raw_line) {
            let mut spans = vec![Span::styled(
                format!("  {num}. "),
                base_style.fg(Color::DarkGray),
            )];
            spans.extend(style_inline(content, base_style));
            result.push(Line::from(spans));
        } else if let Some(content) = is_blockquote(raw_line) {
            let mut spans = vec![Span::styled(
                "| ".to_string(),
                base_style.fg(Color::DarkGray),
            )];
            spans.extend(style_inline(
                content,
                base_style.add_modifier(Modifier::DIM),
            ));
            result.push(Line::from(spans));
        } else {
            let spans = style_inline(raw_line, base_style);
            result.push(Line::from(spans));
        }
    }

    result
}

fn style_inline(text: &str, base: Style) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;
    let mut bold = false;

    while i < len {
        if i + 1 < len
            && ((chars[i] == '*' && chars[i + 1] == '*')
                || (chars[i] == '_' && chars[i + 1] == '_'))
        {
            if !current.is_empty() {
                let style = if bold {
                    base.add_modifier(Modifier::BOLD)
                } else {
                    base
                };
                spans.push(Span::styled(std::mem::take(&mut current), style));
            }
            bold = !bold;
            i += 2;
            continue;
        }

        if chars[i] == '`' {
            if !current.is_empty() {
                let style = if bold {
                    base.add_modifier(Modifier::BOLD)
                } else {
                    base
                };
                spans.push(Span::styled(std::mem::take(&mut current), style));
            }
            i += 1;
            let mut code = String::new();
            while i < len && chars[i] != '`' {
                code.push(chars[i]);
                i += 1;
            }
            if i < len {
                i += 1;
            }
            spans.push(Span::styled(code, base.fg(Color::Yellow)));
            continue;
        }

        if chars[i] == '[' {
            if let Some((link_text, url_end)) = try_parse_link(&chars, i) {
                if !current.is_empty() {
                    let style = if bold {
                        base.add_modifier(Modifier::BOLD)
                    } else {
                        base
                    };
                    spans.push(Span::styled(std::mem::take(&mut current), style));
                }
                spans.push(Span::styled(link_text, base.fg(Color::Cyan)));
                i = url_end;
                continue;
            }
        }

        current.push(chars[i]);
        i += 1;
    }

    if !current.is_empty() {
        let style = if bold {
            base.add_modifier(Modifier::BOLD)
        } else {
            base
        };
        spans.push(Span::styled(current, style));
    }

    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base));
    }

    spans
}

fn try_parse_link(chars: &[char], start: usize) -> Option<(String, usize)> {
    if chars.get(start) != Some(&'[') {
        return None;
    }
    let mut i = start + 1;
    let mut link_text = String::new();
    while i < chars.len() && chars[i] != ']' {
        link_text.push(chars[i]);
        i += 1;
    }
    if i >= chars.len() {
        return None;
    }
    i += 1;
    if i >= chars.len() || chars[i] != '(' {
        return None;
    }
    i += 1;
    while i < chars.len() && chars[i] != ')' {
        i += 1;
    }
    if i >= chars.len() {
        return None;
    }
    i += 1;
    Some((link_text, i))
}

fn is_code_fence(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("```")
}

fn strip_header_prefix(line: &str) -> Option<(u8, &str)> {
    if let Some(rest) = line.strip_prefix("### ") {
        Some((3, rest))
    } else if let Some(rest) = line.strip_prefix("## ") {
        Some((2, rest))
    } else if let Some(rest) = line.strip_prefix("# ") {
        Some((1, rest))
    } else {
        None
    }
}

fn is_bullet(line: &str) -> Option<&str> {
    if let Some(rest) = line.strip_prefix("- ") {
        Some(rest)
    } else if let Some(rest) = line.strip_prefix("* ") {
        Some(rest)
    } else {
        None
    }
}

fn is_numbered(line: &str) -> Option<(usize, &str)> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    if line[i..].starts_with(". ") {
        let num: usize = line[..i].parse().ok()?;
        Some((num, &line[i + 2..]))
    } else {
        None
    }
}

fn is_blockquote(line: &str) -> Option<&str> {
    line.strip_prefix("> ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn plain_text_unchanged() {
        let lines = render_markdown("Hello world", Style::default());
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "Hello world");
    }

    #[test]
    fn bold_text_has_bold_modifier() {
        let lines = render_markdown("This is **bold** text", Style::default());
        assert_eq!(lines.len(), 1);
        let spans = &lines[0].spans;
        assert!(spans.len() >= 3);
        assert!(spans[1].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(spans[1].content.as_ref(), "bold");
    }

    #[test]
    fn code_block() {
        let text = "before\n```rust\nfn main() {}\n```\nafter";
        let lines = render_markdown(text, Style::default());
        assert_eq!(lines.len(), 5);
        assert_eq!(line_text(&lines[0]), "before");
        assert_eq!(lines[2].spans[0].style.fg, Some(Color::Rgb(180, 180, 180)));
        assert_eq!(line_text(&lines[4]), "after");
    }

    #[test]
    fn header_level_1() {
        let lines = render_markdown("# My Header", Style::default());
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Cyan));
        assert!(lines[0].spans[0].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn bullet_list() {
        let lines = render_markdown("- item one\n- item two", Style::default());
        assert_eq!(lines.len(), 2);
        assert!(line_text(&lines[0]).starts_with("  - "));
    }

    #[test]
    fn link_shows_text() {
        let lines = render_markdown("See [docs](https://example.com)", Style::default());
        let spans = &lines[0].spans;
        let link_span = spans.iter().find(|s| s.style.fg == Some(Color::Cyan)).unwrap();
        assert_eq!(span_text(link_span), "docs");
    }

    #[test]
    fn inline_code_has_yellow() {
        let lines = render_markdown("Use `cargo build` here", Style::default());
        let spans = &lines[0].spans;
        let code_span = spans.iter().find(|s| s.style.fg == Some(Color::Yellow)).unwrap();
        assert_eq!(span_text(code_span), "cargo build");
    }

    fn span_text<'a>(span: &'a Span<'a>) -> &'a str {
        span.content.as_ref()
    }
}
