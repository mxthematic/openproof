use openproof_protocol::{ProofNodeKind, ProofQuestionOption, ProofQuestionState};

#[derive(Debug, Clone)]
pub struct ParsedAssistantNode {
    pub kind: ProofNodeKind,
    pub label: String,
    pub statement: String,
}

#[derive(Debug, Clone, Default)]
pub struct ParsedAssistantOutput {
    pub title: Option<String>,
    pub problem: Option<String>,
    pub formal_target: Option<String>,
    pub accepted_target: Option<String>,
    pub phase: Option<String>,
    pub search_status: Option<String>,
    pub assumptions: Vec<String>,
    pub paper_notes: Vec<String>,
    pub paper_tex: Option<String>,
    pub next_steps: Vec<String>,
    pub lean_snippets: Vec<String>,
    pub created_nodes: Vec<ParsedAssistantNode>,
    pub question: Option<ProofQuestionState>,
}

pub fn parse_assistant_output(text: &str) -> ParsedAssistantOutput {
    let mut parsed = ParsedAssistantOutput {
        lean_snippets: extract_lean_code_blocks(text),
        paper_tex: extract_latex_block(text),
        ..ParsedAssistantOutput::default()
    };
    let mut question_prompt: Option<String> = None;
    let mut question_options: Vec<ProofQuestionOption> = Vec::new();
    let mut recommended_option_id: Option<String> = None;

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(value) = strip_prefix_case_insensitive(line, "TITLE:") {
            parsed.title = Some(value.to_string());
            continue;
        }
        if let Some(value) = strip_prefix_case_insensitive(line, "PROBLEM:") {
            parsed.problem = Some(value.to_string());
            continue;
        }
        if let Some(value) = strip_prefix_case_insensitive(line, "FORMAL_TARGET:") {
            parsed.formal_target = Some(value.to_string());
            continue;
        }
        if let Some(value) = strip_prefix_case_insensitive(line, "ACCEPTED_TARGET:") {
            parsed.accepted_target = Some(value.to_string());
            continue;
        }
        if let Some(value) = strip_prefix_case_insensitive(line, "PHASE:") {
            parsed.phase = Some(value.to_lowercase());
            continue;
        }
        if let Some(value) = strip_prefix_case_insensitive(line, "SEARCH:") {
            parsed.search_status = Some(value.to_string());
            continue;
        }
        if let Some(value) = strip_prefix_case_insensitive(line, "STATUS:") {
            parsed.search_status = Some(value.to_string());
            continue;
        }
        if let Some(value) = strip_prefix_case_insensitive(line, "QUESTION:") {
            question_prompt = Some(value.to_string());
            continue;
        }
        if let Some(value) = strip_prefix_case_insensitive(line, "RECOMMENDED_OPTION:") {
            recommended_option_id = Some(value.to_string());
            continue;
        }
        if let Some(value) = strip_prefix_case_insensitive(line, "ASSUMPTION:") {
            parsed.assumptions.push(value.to_string());
            continue;
        }
        if let Some(value) = strip_prefix_case_insensitive(line, "PAPER:") {
            parsed.paper_notes.push(value.to_string());
            continue;
        }
        if let Some(value) = strip_prefix_case_insensitive(line, "PAPER_NOTE:") {
            parsed.paper_notes.push(value.to_string());
            continue;
        }
        if let Some(value) = strip_prefix_case_insensitive(line, "NEXT:") {
            parsed.next_steps.push(value.to_string());
            continue;
        }
        if let Some((label, statement)) = parse_labeled_statement(line, "THEOREM:") {
            parsed.created_nodes.push(ParsedAssistantNode {
                kind: ProofNodeKind::Theorem,
                label,
                statement,
            });
            continue;
        }
        if let Some((label, statement)) = parse_labeled_statement(line, "LEMMA:") {
            parsed.created_nodes.push(ParsedAssistantNode {
                kind: ProofNodeKind::Lemma,
                label,
                statement,
            });
            continue;
        }
        if let Some((label, statement)) = parse_labeled_statement(line, "LEMMA_CANDIDATE:") {
            parsed.created_nodes.push(ParsedAssistantNode {
                kind: ProofNodeKind::Lemma,
                label,
                statement,
            });
            continue;
        }
        if let Some(option) = parse_question_option(line) {
            question_options.push(option);
            continue;
        }
        if let Some((option_id, target)) = parse_option_target(line) {
            if let Some(existing) = question_options.iter_mut().find(|option| option.id == option_id) {
                existing.formal_target = target;
            } else {
                question_options.push(ProofQuestionOption {
                    id: option_id.clone(),
                    label: option_id,
                    summary: String::new(),
                    formal_target: target,
                });
            }
        }
    }

    if let Some(prompt) = question_prompt.filter(|item| !item.trim().is_empty()) {
        let options = question_options
            .into_iter()
            .filter(|option| !option.formal_target.trim().is_empty())
            .collect::<Vec<_>>();
        if !options.is_empty() {
            parsed.question = Some(ProofQuestionState {
                prompt,
                options,
                recommended_option_id,
                answer_text: None,
                status: "open".to_string(),
            });
        }
    }

    parsed
}

pub fn extract_lean_code_block(content: &str) -> Option<String> {
    let start = content.find("```lean")?;
    let rest = &content[start + "```lean".len()..];
    let rest = rest.strip_prefix('\n').unwrap_or(rest);
    let end = rest.find("```")?;
    let block = rest[..end].trim();
    if block.is_empty() {
        None
    } else {
        Some(block.to_string())
    }
}

pub fn extract_lean_code_blocks(content: &str) -> Vec<String> {
    let mut snippets = Vec::new();
    let mut rest = content;
    while let Some(start) = rest.find("```lean") {
        let after_start = &rest[start + "```lean".len()..];
        let after_start = after_start.strip_prefix('\n').unwrap_or(after_start);
        let Some(end) = after_start.find("```") else {
            break;
        };
        let block = after_start[..end].trim();
        if !block.is_empty() {
            snippets.push(block.to_string());
        }
        rest = &after_start[end + "```".len()..];
    }
    snippets
}

pub fn extract_latex_block(content: &str) -> Option<String> {
    let start = content.find("```latex")?;
    let after_start = &content[start + "```latex".len()..];
    let after_start = after_start.strip_prefix('\n').unwrap_or(after_start);
    let end = after_start.find("```")?;
    let block = after_start[..end].trim();
    if block.is_empty() {
        None
    } else {
        Some(block.to_string())
    }
}

pub fn derive_goal_label(title: &str) -> String {
    let mut label = title
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect::<String>();
    while label.contains("__") {
        label = label.replace("__", "_");
    }
    label = label.trim_matches('_').to_string();
    if label.is_empty() {
        "goal".to_string()
    } else if label.chars().next().map(|ch| ch.is_ascii_digit()).unwrap_or(false) {
        format!("goal_{label}")
    } else {
        label
    }
}

fn strip_prefix_case_insensitive<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let line_upper = line.to_ascii_uppercase();
    let prefix_upper = prefix.to_ascii_uppercase();
    if line_upper.starts_with(&prefix_upper) {
        Some(line[prefix.len()..].trim())
    } else {
        None
    }
}

fn parse_labeled_statement(line: &str, prefix: &str) -> Option<(String, String)> {
    let body = strip_prefix_case_insensitive(line, prefix)?;
    let (label, statement) = body.split_once("::")?;
    let label = label.trim();
    let statement = statement.trim();
    if label.is_empty() || statement.is_empty() {
        None
    } else {
        Some((label.to_string(), statement.to_string()))
    }
}

fn parse_question_option(line: &str) -> Option<ProofQuestionOption> {
    let body = strip_prefix_case_insensitive(line, "OPTION:")?;
    let parts = body.split('|').map(str::trim).collect::<Vec<_>>();
    if parts.len() < 2 {
        return None;
    }
    let id = parts[0];
    let label = parts[1];
    if id.is_empty() || label.is_empty() {
        return None;
    }
    Some(ProofQuestionOption {
        id: id.to_string(),
        label: label.to_string(),
        summary: parts.get(2).copied().unwrap_or_default().to_string(),
        formal_target: String::new(),
    })
}

fn parse_option_target(line: &str) -> Option<(String, String)> {
    let body = strip_prefix_case_insensitive(line, "OPTION_TARGET:")
        .or_else(|| strip_prefix_case_insensitive(line, "FORMAL_TARGET_OPTION:"))?;
    let (id, target) = body.split_once("::")?;
    let id = id.trim();
    let target = target.trim();
    if id.is_empty() || target.is_empty() {
        None
    } else {
        Some((id.to_string(), target.to_string()))
    }
}
