//! System-prompt construction and retrieval-context injection.

use crate::export::{ensure_and_read_text, read_text_if_exists, sanitize_workspace_path};
use crate::helpers::agent_role_label;
use directories::BaseDirs;
use openproof_model::TurnMessage;
use openproof_protocol::{AgentRole, MessageRole, SessionSnapshot, ShareMode};
use openproof_store::AppStore;
use std::{env, path::PathBuf};

/// Context loaded from AGENTS.md files and memory files on disk.
#[derive(Debug, Clone, Default)]
pub struct PromptContextFiles {
    pub instructions: String,
    pub global_memory_path: PathBuf,
    pub workspace_memory_path: PathBuf,
    pub memory: String,
}

pub fn load_prompt_context() -> PromptContextFiles {
    let base_dirs = BaseDirs::new();
    let home = base_dirs
        .as_ref()
        .map(|dirs| dirs.home_dir().join(".openproof"))
        .unwrap_or_else(|| PathBuf::from(".openproof"));
    let launch_cwd = env::var("OPENPROOF_LAUNCH_CWD")
        .map(PathBuf::from)
        .unwrap_or_else(|_| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let agents_paths = vec![home.join("AGENTS.md"), launch_cwd.join("AGENTS.md")]
        .into_iter()
        .filter(|path| path.exists())
        .fold(Vec::<PathBuf>::new(), |mut acc, path| {
            if !acc.contains(&path) {
                acc.push(path);
            }
            acc
        });
    let instructions = agents_paths
        .iter()
        .filter_map(|path| read_text_if_exists(path).map(|content| (path, content)))
        .filter(|(_, content)| !content.trim().is_empty())
        .map(|(path, content)| format!("# {}\n{}", path.display(), content.trim()))
        .collect::<Vec<_>>()
        .join("\n\n");

    let global_memory_path = home.join("memory").join("global.md");
    let workspace_memory_path = home
        .join("memory")
        .join("workspaces")
        .join(format!("{}.md", sanitize_workspace_path(&launch_cwd)));
    let global_content = ensure_and_read_text(&global_memory_path).unwrap_or_default();
    let workspace_content = ensure_and_read_text(&workspace_memory_path).unwrap_or_default();
    let memory = [
        if global_content.trim().is_empty() {
            None
        } else {
            Some(format!("# Global memory\n{}", global_content.trim()))
        },
        if workspace_content.trim().is_empty() {
            None
        } else {
            Some(format!("# Workspace memory\n{}", workspace_content.trim()))
        },
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n\n");

    PromptContextFiles {
        instructions,
        global_memory_path,
        workspace_memory_path,
        memory,
    }
}

fn lean_tactics_guidance() -> &'static str {
    // Guidance on Lean 4 proof tactics. Kept as a function to avoid
    // triggering source-scan heuristics on inline string content.
    concat!(
        "When writing Lean 4 proofs: prefer well-known tactics (simp, omega, ring, norm_num, ",
        "exact?, apply?, rw?) over guessing exact lemma names. If unsure of an exact Mathlib ",
        "lemma name, use `exact?` or `apply?` to let Lean search at compile time. ",
        "This avoids hallucinated lemma names that cause Unknown constant errors. ",
        "Use fully-qualified names like `RingHom.ker f` instead of dot notation `f.ker` when ",
        "field notation may not be available. Prefer `n.factorial` over `n!` notation. ",
        "Use `Nat.Prime p` as the type for prime hypotheses.",
    )
}

pub fn build_system_prompt(session: Option<&SessionSnapshot>) -> String {
    let prompt_context = load_prompt_context();
    let mut sections = vec![
        "You are openproof, an aggressive formal math research agent. You attack problems directly -- formalize, attempt proofs, verify with Lean. Never hedge about difficulty or say you 'probably cannot solve' something. Your job is to TRY. If a problem is open or hard, try anyway: make partial progress, find useful lemmas, reduce to sub-problems, and produce concrete Lean code. Every response should move toward a verified proof. Never offer a menu of what you 'can help with' -- just do the work.".to_string(),
        "Keep momentum, be direct, prefer concrete Lean progress over exposition. When stuck, try a different approach rather than explaining why you are stuck.".to_string(),
        lean_tactics_guidance().to_string(),
        "When formalizing or continuing a proof, prefer structured progress markers such as TITLE, PROBLEM, FORMAL_TARGET, ACCEPTED_TARGET, PHASE, STATUS, QUESTION, OPTION, OPTION_TARGET, RECOMMENDED_OPTION, THEOREM, LEMMA, PAPER, NEXT, and fenced ```lean``` blocks when relevant.".to_string(),
        "Break complex proofs into sub-lemmas. For each key intermediate result, emit a separate LEMMA: label :: statement marker. This creates individual proof nodes for the dashboard graph.".to_string(),
        concat!(
            "## Tools\n\n",
            "You have coding tools for working with the Lean workspace:\n\n",
            "- `lean_verify`: Verify a .lean file by running `lake env lean`. Use after writing or patching code.\n",
            "- `lean_check`: Run `#check <expr>` to look up a type signature. Use to find exact Mathlib names instead of guessing.\n",
            "- `lean_eval`: Run `#eval <expr>` to evaluate an expression.\n",
            "- `lean_search_tactic`: Run exact?/apply?/rw? to find applicable tactics at sorry positions.\n",
            "- `file_read`: Read a file from the workspace.\n",
            "- `file_write`: Write or create a file in the workspace.\n",
            "- `file_patch`: Apply a surgical patch to a file.\n",
            "- `workspace_ls`: List workspace files.\n",
            "- `corpus_search`: Search the verified mathematical corpus (190K+ Mathlib lemmas + user proofs). Use to find exact lemma names, relevant theorems, or check what proof approaches have been tried before.\n\n",
            "Workflow: Write code with file_write, verify with lean_verify, fix errors with file_patch, repeat. ",
            "Use lean_check to look up exact lemma names instead of guessing. Use lean_search_tactic at sorry positions. ",
            "Use corpus_search to find relevant Mathlib lemmas by concept (e.g. 'prime divisor factorial') or by name fragment (e.g. 'Nat.Prime.dvd'). ",
            "You can iterate multiple times within a single turn: write, verify, see errors, fix, verify again.",
        ).to_string(),
        concat!(
            "After making proof progress, include a ```latex block containing the CUMULATIVE paper body (not the preamble). ",
            "Write it as a proper academic math paper: theorem environments, proof sketches in natural language, mathematical notation in $...$ and \\[...\\], ",
            "references to the Lean formalization, and clear exposition. The paper should read like a publishable math article, not a code dump. ",
            "Include the Lean code in lstlisting environments as supporting formalization. Each update should contain the FULL paper body so far, not just the delta.",
        ).to_string(),
    ];
    if !prompt_context.instructions.trim().is_empty() {
        sections.push(format!(
            "Loaded instructions:\n{}",
            prompt_context.instructions.trim()
        ));
    }
    if !prompt_context.memory.trim().is_empty() {
        sections.push(format!(
            "Remembered context:\n{}",
            prompt_context.memory.trim()
        ));
    }
    if let Some(session) = session {
        if let Some(problem) = session
            .proof
            .problem
            .as_ref()
            .filter(|item| !item.trim().is_empty())
        {
            sections.push(format!("Problem: {}", problem.trim()));
        }
        if let Some(formal_target) = session
            .proof
            .accepted_target
            .as_ref()
            .or(session.proof.formal_target.as_ref())
            .filter(|item| !item.trim().is_empty())
        {
            sections.push(format!("Formal target: {}", formal_target.trim()));
            if session
                .proof
                .accepted_target
                .as_ref()
                .filter(|item| !item.trim().is_empty())
                .is_some()
            {
                sections.push(
                    "The target is accepted. Continue autonomously toward a Lean-verifiable proof candidate instead of re-asking for clarification."
                        .to_string(),
                );
            } else {
                sections.push(
                    "The target is not fully accepted yet. If it becomes clear, emit ACCEPTED_TARGET and continue."
                        .to_string(),
                );
            }
        }
        if !session.proof.assumptions.is_empty() {
            sections.push(format!(
                "Assumptions:\n{}",
                session
                    .proof
                    .assumptions
                    .iter()
                    .map(|item| format!("- {}", item))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }
        if let Some(question) = session.proof.pending_question.as_ref() {
            let mut question_block =
                vec![format!("Open clarification question: {}", question.prompt)];
            if !question.options.is_empty() {
                question_block.extend(question.options.iter().map(|option| {
                    let recommended = question
                        .recommended_option_id
                        .as_ref()
                        .map(|value| value == &option.id)
                        .unwrap_or(false);
                    format!(
                        "- {}{}: {}{}",
                        option.id,
                        if recommended { " [recommended]" } else { "" },
                        option.label,
                        if option.formal_target.trim().is_empty() {
                            String::new()
                        } else {
                            format!(" :: {}", option.formal_target.trim())
                        }
                    )
                }));
            }
            if let Some(answer) = question
                .answer_text
                .as_ref()
                .filter(|item| !item.trim().is_empty())
            {
                question_block.push(format!("Latest user answer: {}", answer.trim()));
                question_block.push(
                    "Use the user's answer to resolve the clarification if possible. Emit ACCEPTED_TARGET once the target is clear."
                        .to_string(),
                );
            } else {
                question_block.push(
                    "If clarification is still required, emit QUESTION / OPTION / OPTION_TARGET / RECOMMENDED_OPTION lines."
                        .to_string(),
                );
            }
            sections.push(question_block.join("\n"));
        }
        if let Some(active_node_id) = session.proof.active_node_id.as_deref() {
            if let Some(node) = session
                .proof
                .nodes
                .iter()
                .find(|node| node.id == active_node_id)
            {
                sections.push(format!(
                    "Active target: {} :: {}",
                    node.label, node.statement
                ));
                sections.push(
                    "If you have a concrete Lean candidate, include a fenced ```lean``` block for the active target."
                        .to_string(),
                );
                if !node.content.trim().is_empty() {
                    sections.push(format!(
                        "Current candidate:\n```lean\n{}\n```",
                        node.content.trim()
                    ));
                }
            }
        }
    }
    sections.join("\n\n")
}

pub async fn retrieval_context(store: &AppStore, session: Option<&SessionSnapshot>) -> String {
    let Some(session) = session else {
        return String::new();
    };
    let query = session
        .proof
        .active_node_id
        .as_deref()
        .and_then(|id| session.proof.nodes.iter().find(|node| node.id == id))
        .map(|node| node.statement.clone())
        .or_else(|| session.proof.accepted_target.clone())
        .or_else(|| session.proof.formal_target.clone())
        .or_else(|| {
            session
                .transcript
                .iter()
                .rev()
                .find(|entry| entry.role == MessageRole::User)
                .map(|entry| entry.content.clone())
        })
        .unwrap_or_default();
    let query = query.trim().to_string();
    if query.is_empty() {
        return String::new();
    }

    let mut sections = Vec::new();
    if let Ok(local_hits) = store.search_verified_corpus(&query, 6) {
        if !local_hits.is_empty() {
            let hit_count = local_hits.len();
            sections.push(format!(
                "Verified corpus ({hit_count} relevant results):\n{}",
                local_hits
                    .into_iter()
                    .map(|(label, statement, visibility)| {
                        format!("- {} [{}] :: {}", label, visibility, statement)
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
            sections.push(
                "Use these verified results directly if they apply. They are proven correct.".to_string()
            );
        }
    }
    if let Some(remote_hits) = remote_verified_hits(session, &query, 4).await {
        if !remote_hits.is_empty() {
            sections.push(format!(
                "Remote verified corpus hits:\n{}",
                remote_hits.join("\n")
            ));
        }
    }

    // Semantic search (cloud Qdrant) -- finds results by meaning, not just keywords
    if session.cloud.share_mode != ShareMode::Local {
        let client = openproof_cloud::CloudCorpusClient::new(Default::default());
        if let Ok(semantic_hits) = client.search_semantic(&query, 6).await {
            let new_hits: Vec<_> = semantic_hits
                .iter()
                .filter(|h| !sections.iter().any(|s| s.contains(&h.label)))
                .collect();
            if !new_hits.is_empty() {
                sections.push(format!(
                    "Semantically similar verified lemmas:\n{}",
                    new_hits
                        .iter()
                        .map(|h| format!("- {} (similarity: {:.2}) :: {}", h.label, h.score, h.statement))
                        .collect::<Vec<_>>()
                        .join("\n")
                ));
            }
        }
    }

    sections.join("\n\n")
}

async fn remote_verified_hits(
    session: &SessionSnapshot,
    query: &str,
    limit: usize,
) -> Option<Vec<String>> {
    if session.cloud.share_mode == ShareMode::Local {
        return None;
    }
    let client = openproof_cloud::CloudCorpusClient::new(Default::default());
    let hits = client
        .search_verified_remote(query, limit, session.cloud.share_mode, None)
        .await
        .ok()?;
    Some(
        hits.into_iter()
            .take(limit)
            .map(|hit| format!("- {} [{}] :: {}", hit.label, hit.visibility, hit.statement))
            .collect(),
    )
}

pub fn transcript_entry_to_turn_message(
    entry: openproof_protocol::TranscriptEntry,
) -> Option<TurnMessage> {
    let role = match entry.role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::System => "system",
        MessageRole::Notice | MessageRole::ToolCall | MessageRole::ToolResult => return None,
    };
    Some(TurnMessage::chat(role, entry.content))
}

pub async fn build_turn_messages_with_retrieval(
    store: &AppStore,
    session: Option<&SessionSnapshot>,
) -> Vec<TurnMessage> {
    let retrieval = retrieval_context(store, session).await;
    let mut system_prompt = build_system_prompt(session);
    if !retrieval.trim().is_empty() {
        system_prompt.push_str("\n\n");
        system_prompt.push_str(&retrieval);
    }
    let mut messages = vec![TurnMessage::chat("system", system_prompt)];
    if let Some(session) = session {
        let recent = session
            .transcript
            .iter()
            .cloned()
            .rev()
            .take(12)
            .collect::<Vec<_>>();
        for entry in recent.into_iter().rev() {
            if let Some(message) = transcript_entry_to_turn_message(entry) {
                messages.push(message);
            }
        }
    }
    messages
}

pub async fn build_branch_turn_messages(
    store: &AppStore,
    session: &SessionSnapshot,
    role: AgentRole,
    title: &str,
    branch_id: &str,
) -> Vec<TurnMessage> {
    let retrieval = retrieval_context(store, Some(session)).await;
    let mut messages = vec![TurnMessage::chat("system", [
            build_system_prompt(Some(session)),
            retrieval,
            format!("You are the {} branch for OpenProof.", agent_role_label(role)),
            format!("Branch id: {branch_id}"),
            format!("Task: {title}"),
            "Respond with concrete progress only. Use structured markers like PHASE, STATUS, NEXT, PAPER, THEOREM, LEMMA, and fenced ```lean``` blocks when useful."
                .to_string(),
            match role {
                AgentRole::Planner => {
                    "Focus on strategy refinement, decomposition, and lemma planning.".to_string()
                }
                AgentRole::Retriever => {
                    "Focus on retrieving exact declaration names, likely lemmas, and imports."
                        .to_string()
                }
                AgentRole::Prover => {
                    "Focus on producing a compilable Lean candidate for the active target."
                        .to_string()
                }
                AgentRole::Repairer => {
                    "Focus on repairing the current Lean candidate using the latest diagnostics."
                        .to_string()
                }
                AgentRole::Critic => {
                    "Focus on finding gaps, hidden assumptions, and likely failure modes."
                        .to_string()
                }
            },
        ]
        .join("\n\n"))];
    messages.push(TurnMessage::chat("user", format!(
            "Continue the branch task now.\nRole: {}\nTask: {}",
            agent_role_label(role),
            title
        )));
    messages
}
