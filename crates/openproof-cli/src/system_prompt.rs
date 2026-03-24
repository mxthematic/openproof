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

fn tools_and_workflow_section() -> &'static str {
    concat!(
        "## Tools & Workspace\n\n",
        "You have a persistent Lean 4 workspace. You work like a coding agent building a Lean codebase: ",
        "read files, write new files, patch existing files, verify, fix errors, iterate. ",
        "The workspace persists across turns. Treat it like a real project, not a scratch pad.\n\n",

        "### File tools\n",
        "- `file_write(path, content)`: Create a NEW file. Use ONLY for files that don't exist yet.\n",
        "- `file_read(path)`: Read a file with line numbers. ALWAYS read before patching.\n",
        "- `file_patch(path, patch)`: Apply a surgical diff. This is your PRIMARY editing tool. ",
        "Format: *** Begin Patch / *** Update File: path / @@ context line / -old / +new / *** End Patch.\n",
        "- `workspace_ls()`: List all files in the workspace.\n\n",

        "### How to edit files (CRITICAL -- read this)\n",
        "You work like Claude Code. The rules:\n",
        "1. NEVER use file_write on a file that already exists. Use file_patch instead.\n",
        "2. ALWAYS file_read before file_patch so you see line numbers and context.\n",
        "3. When lean_verify fails at line N, file_read the file, find line N, patch JUST that spot.\n",
        "4. Never rewrite a file from scratch if it has any working code. Patch the broken parts only.\n",
        "5. Organize complex proofs into multiple files:\n",
        "   - `Defs.lean` -- definitions, structures, basic API\n",
        "   - `Helpers.lean` -- utility lemmas, supporting results\n",
        "   - `Main.lean` -- main theorem statement and proof\n",
        "   - `Paper.tex` -- LaTeX paper with natural language proof and formalization\n",
        "   - Use `import «Defs»` / `import «Helpers»` between files\n",
        "6. Each file should be focused and under 200 lines. Split when a file gets large.\n",
        "7. Always maintain Paper.tex alongside the Lean code. It should contain:\n",
        "   - A proper LaTeX document with theorem/proof environments\n",
        "   - The mathematical proof in natural language\n",
        "   - The Lean formalization in lstlisting environments\n",
        "   - Update it with file_patch as the proof evolves.\n\n",

        "### Lean tools\n",
        "- `lean_verify(file)`: Compile a .lean file. Returns errors, warnings, goals. ALWAYS verify after patching.\n",
        "- `lean_goals(file)`: Extract structured proof goals at each sorry. USE THIS after lean_verify to see ",
        "exactly what needs to be proved (hypotheses, target type). Critical for understanding what tactic to try.\n",
        "- `lean_screen_tactics(file, line, tactics)`: Try multiple tactics at a sorry WITHOUT modifying the file. ",
        "Pass an array of 5-10 tactics. Returns which ones work. MUCH faster than patch+recompile for each. ",
        "Example: `lean_screen_tactics({\"file\":\"Main.lean\",\"line\":25,\"tactics\":[\"simp\",\"omega\",\"ring\",\"exact?\",\"apply?\"]})`\n",
        "- `lean_check(expr)`: Look up the type of a Mathlib lemma.\n",
        "- `lean_search_tactic(tactic, file, line)`: Run exact?/apply?/rw? at a sorry. Lean searches Mathlib.\n\n",
        "THE CORE LOOP (repeat until all sorrys are filled):\n",
        "  lean_goals -> lean_screen_tactics -> file_patch -> lean_verify\n",
        "This is the most important workflow in the entire system. Every sorry goes through this loop. ",
        "Do not skip lean_goals -- you need to see the exact goal before trying tactics. ",
        "Do not skip lean_screen_tactics -- batch-testing is 5x faster than patch+recompile for each tactic.\n\n",

        "### Research & computation tools\n",
        "- `corpus_search(query)`: Search 190K+ verified Mathlib declarations + user proofs.\n",
        "- `shell_run(command)`: Run a shell command (sage, python3, etc.) for computation. ",
        "Use sage for symbolic math, bounds checking, series evaluation, combinatorial enumeration. ",
        "Example: `shell_run({\"command\": \"sage -c 'print(sum(1/fibonacci(k) for k in range(2,20)).n())'\"})`. ",
        "Use python3 for numerical exploration.\n",
        "- Web search (built-in): Search for papers, ArXiv preprints, MathOverflow.\n\n",

        "### PROVING WORKFLOW (follow this EXACTLY)\n\n",
        "**Step 1 -- ORIENT (1-2 tool calls max):** `workspace_ls` + `file_read` to see what exists. ",
        "One `corpus_search` to check if the result is already in Mathlib. If it exists, use `exact` or `apply`.\n\n",
        "**Step 2 -- WRITE SKELETON (immediate):** Create Main.lean with the theorem statement ",
        "and a sorry-skeleton using `have` chains. `lean_verify` to confirm it compiles. ",
        "This should happen within your first 4 tool calls. Do NOT spend more time researching.\n\n",
        "**Step 3 -- FILL SORRYS (90% of your time here):** For each sorry:\n",
        "  a. `lean_goals` to see the exact goal and hypotheses\n",
        "  b. `lean_screen_tactics` to batch-test 8-10 tactics (simp, omega, ring, exact?, apply?, linarith, norm_num, aesop, decide, tauto)\n",
        "  c. If a tactic works: `file_patch` to apply it, `lean_verify` to confirm\n",
        "  d. If no tactic works: `corpus_search` for THAT SPECIFIC goal type, then try `lean_search_tactic`\n",
        "  e. If still stuck: decompose THIS sorry into sub-`have` steps and repeat from (a)\n\n",
        "**Step 4 -- ITERATE:** After each `lean_verify`, immediately address the next error or sorry. ",
        "Never stop to write prose between verify cycles. Keep the tight loop going.\n\n",
        "### ANTI-PATTERNS (avoid these)\n",
        "- Do NOT do more than 2 `corpus_search` calls before writing your first .lean file.\n",
        "- Do NOT use `shell_run` to explore the filesystem. Use `workspace_ls`.\n",
        "- Do NOT search for the same thing twice.\n",
        "- Do NOT explain what you plan to do. Just do it.\n",
        "- Do NOT `lean_check` random names hoping one exists. Use `exact?`/`apply?` instead.\n",
        "- Do NOT read Mathlib source files. Use `corpus_search` and `lean_check`.\n",
        "- Do NOT do research as a substitute for writing code. Research serves the code, not the other way around.\n\n",

        "### Workflow\n",
        "If the workspace is EMPTY (no .lean files yet):\n",
        "1. Research the proof technique (web search, shell_run, corpus_search).\n",
        "2. `file_write` to create files with informal comments + sorry skeleton.\n",
        "3. `lean_verify` to check the skeleton compiles.\n\n",
        "If the workspace already HAS files:\n",
        "1. `workspace_ls` then `file_read` to see current code.\n",
        "2. `file_patch` to fill a sorry or fix an error.\n",
        "3. `lean_verify` after each patch.\n\n",
        "Iterate: file_read -> file_patch -> lean_verify -> repeat.\n\n",

        "RULES:\n",
        "- file_patch is your primary tool. file_write is ONLY for creating files that don't exist yet.\n",
        "- NEVER create a new file when you could patch an existing one.\n",
        "- Always file_read before file_patch.\n",
        "- Decompose hard proofs into `have` chains with sorry. Fill sorrys one by one.\n",
        "- Always lean_verify after changes.",
    )
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
        concat!(
            "You are openproof, an aggressive formal math coding agent. You are a CODING agent, not a research assistant. ",
            "Your primary output is Lean code, not text. You attack problems by writing code, verifying it, fixing errors, and iterating. ",
            "Never hedge about difficulty or say you 'probably cannot solve' something. Your job is to TRY. ",
            "If a problem is open or hard, try anyway: write a sorry-skeleton, fill what you can, reduce to sub-problems. ",
            "Every response should produce or modify Lean code. Never offer a menu of what you 'can help with' -- just write code.\n\n",
            "CODE-FIRST RULE: If you haven't written or modified a .lean file in your last 3 tool calls, you are off track. ",
            "Stop researching and write code NOW. Spend 80% of your tool calls on file_write/file_patch/lean_verify ",
            "and 20% on research (corpus_search, lean_check, shell_run).",
        ).to_string(),
        concat!(
            "CRITICAL: You must prove the EXACT problem the user stated. Do NOT substitute an easier variant. ",
            "If the user asks to 'improve the constant 0.382', you must produce a theorem that yields a constant BETTER than 0.382 -- not a theorem about a special case where the conjecture trivially holds. ",
            "If the user asks about an open problem, do NOT prove a known/trivial restricted case and declare success. ",
            "Known easy cases (e.g. Frankl for powerset families, lattice families, or families with a singleton) are NOT acceptable -- they are textbook exercises, not research results. ",
            "The goal is a NOVEL formally verified result. If you can only prove a sub-lemma, say so explicitly and keep working toward the main result.\n\n",
            "NEVER write a vacuous proof. Specifically:\n",
            "- Do NOT assume the conjecture as a hypothesis and prove True.\n",
            "- Do NOT use `axiom` or `constant` to postulate what you need to prove.\n",
            "- The main theorem must have only structural hypotheses (type constraints, finiteness, etc.), not the conclusion repackaged as an assumption.\n",
            "- If your theorem says `(hF : ∀ F, ... → HasFrequencyAtLeast F c) : True`, that is VACUOUS and UNACCEPTABLE.\n",
            "- The proof must DERIVE the conclusion from first principles and Mathlib, not assume it.\n",
            "- NEVER change the theorem statement to make it easier. If Main.lean has a theorem with sorry, ",
            "you must fill that EXACT sorry with a proof. Do NOT replace the theorem with a weaker version. ",
            "Do NOT delete the sorry and replace it with a different theorem you can prove. ",
            "The theorem statement is FIXED. Only the proof (the sorry) can change.",
        ).to_string(),
        "Keep momentum, be direct, prefer concrete Lean progress over exposition. When stuck, try a different approach rather than explaining why you are stuck.".to_string(),
        lean_tactics_guidance().to_string(),
        "When formalizing or continuing a proof, prefer structured progress markers such as TITLE, PROBLEM, FORMAL_TARGET, ACCEPTED_TARGET, PHASE, STATUS, QUESTION, OPTION, OPTION_TARGET, RECOMMENDED_OPTION, THEOREM, LEMMA, PAPER, NEXT, and fenced ```lean``` blocks when relevant.".to_string(),
        "Break complex proofs into sub-lemmas. For each key intermediate result, emit a separate LEMMA: label :: statement marker. This creates individual proof nodes for the dashboard graph.".to_string(),
        tools_and_workflow_section().to_string(),
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
            "You have tools: file_read, file_write, file_patch, lean_verify, lean_check, lean_search_tactic, corpus_search, workspace_ls. \
             Use file_read to see current code. Use file_patch for targeted fixes. Use lean_verify to check. \
             NEVER output lean code in text -- always use file_write or file_patch to modify files. \
             NEVER overwrite a file that has working code -- use file_patch to add/modify specific parts."
                .to_string(),
            match role {
                AgentRole::Planner => {
                    concat!(
                        "Write a brief informal proof sketch (1-2 paragraphs as Lean comments), ",
                        "then IMMEDIATELY create the sorry-skeleton in Lean and lean_verify it. ",
                        "Do not spend more than 5 tool calls on research. The compilable skeleton matters ",
                        "more than the research. Focus on decomposing the theorem into a chain of `have` ",
                        "steps that captures the KEY MATHEMATICAL INSIGHT. The Prover branch will fill the sorrys.",
                    ).to_string()
                }
                AgentRole::Retriever => {
                    "Focus on retrieving exact declaration names, likely lemmas, and imports."
                        .to_string()
                }
                AgentRole::Prover => {
                    concat!(
                        "You are a PROVER. Your ONLY job is to fill sorrys with working Lean code. ",
                        "Do NOT research. Do NOT explain. Write code, verify, fix, repeat. ",
                        "Start by running lean_goals on the file. For each sorry goal: ",
                        "lean_screen_tactics (test simp, omega, ring, exact?, apply?, linarith, norm_num, aesop, decide, tauto) ",
                        "-> file_patch to apply the working tactic -> lean_verify to confirm. ",
                        "If stuck on a goal for more than 3 attempts, decompose it with sub-have steps. ",
                        "Do NOT change theorem statements. Spend ALL your tool calls on the lean_goals/screen_tactics/patch/verify loop.",
                    ).to_string()
                }
                AgentRole::Repairer => {
                    concat!(
                        "Repair the current Lean candidate using the latest diagnostics. ",
                        "For each error: file_read the code, lean_goals to see the goal state, ",
                        "lean_screen_tactics to batch-test fixes, then file_patch to apply the fix. ",
                        "Do NOT output patches as text. Use the file_patch tool. Then lean_verify.",
                    ).to_string()
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

    // Include ALL workspace .lean files so branches see current codebase
    if let Ok(files) = store.list_workspace_files(&session.id) {
        let ws_dir = store.workspace_dir(&session.id);
        let lean_files: Vec<_> = files.iter()
            .filter(|(p, _)| p.ends_with(".lean") && !p.contains("history/"))
            .collect();
        if !lean_files.is_empty() {
            for (path, _) in &lean_files {
                if let Ok(content) = std::fs::read_to_string(ws_dir.join(path)) {
                    if !content.trim().is_empty() && content.lines().count() <= 200 {
                        messages.push(TurnMessage::chat("user", format!(
                            "File: {path} ({} lines):\n```lean\n{content}\n```",
                            content.lines().count()
                        )));
                    }
                }
            }
            messages.push(TurnMessage::chat("user",
                "Build on this code. Use file_patch to modify existing files. Do NOT rewrite from scratch.".to_string()
            ));
        }
    }

    // Include active node status so branches know verification state
    if let Some(node) = session.proof.nodes.iter()
        .find(|n| Some(n.id.as_str()) == session.proof.active_node_id.as_deref())
    {
        if !node.content.trim().is_empty() {
            messages.push(TurnMessage::chat("user", format!(
                "Active node '{}' status: {:?}",
                node.label, node.status
            )));
        }
    }

    messages
}
