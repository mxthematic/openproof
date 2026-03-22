//! Tool definitions for the LLM agent in OpenAI function-calling format.
//!
//! These schemas are included in the API payload so the model can call tools
//! like `lean_verify`, `file_write`, etc. during its agentic loop.

use serde_json::{json, Value};

/// Returns all tool definitions for inclusion in the Responses API payload.
pub fn tool_definitions() -> Vec<Value> {
    vec![
        lean_verify_tool(),
        lean_check_tool(),
        lean_eval_tool(),
        lean_search_tactic_tool(),
        file_read_tool(),
        file_write_tool(),
        file_patch_tool(),
        workspace_ls_tool(),
        corpus_search_tool(),
    ]
}

fn lean_verify_tool() -> Value {
    json!({
        "type": "function",
        "name": "lean_verify",
        "description": "Verify a Lean 4 file by running `lake env lean` on it. Returns compiler diagnostics (errors, warnings, goals). Use after writing or patching code to check correctness.",
        "parameters": {
            "type": "object",
            "properties": {
                "file": {
                    "type": "string",
                    "description": "Relative path to the .lean file in the workspace. Defaults to 'Scratch.lean'.",
                    "default": "Scratch.lean"
                }
            },
            "required": [],
            "additionalProperties": false
        }
    })
}

fn lean_check_tool() -> Value {
    json!({
        "type": "function",
        "name": "lean_check",
        "description": "Run `#check <expr>` in Lean 4 to look up the type of an expression or find the exact name of a Mathlib lemma. Returns the type signature.",
        "parameters": {
            "type": "object",
            "properties": {
                "expr": {
                    "type": "string",
                    "description": "The Lean expression to check, e.g. 'Nat.Prime.dvd_mul' or '@List.map'"
                }
            },
            "required": ["expr"],
            "additionalProperties": false
        }
    })
}

fn lean_eval_tool() -> Value {
    json!({
        "type": "function",
        "name": "lean_eval",
        "description": "Run `#eval <expr>` in Lean 4 to evaluate an expression and see its result. Useful for testing computations.",
        "parameters": {
            "type": "object",
            "properties": {
                "expr": {
                    "type": "string",
                    "description": "The Lean expression to evaluate, e.g. 'Nat.gcd 12 8' or '(List.range 10).filter Nat.Prime'"
                }
            },
            "required": ["expr"],
            "additionalProperties": false
        }
    })
}

fn lean_search_tactic_tool() -> Value {
    json!({
        "type": "function",
        "name": "lean_search_tactic",
        "description": "Run a tactic search (exact?, apply?, or rw?) at a sorry position in a Lean file. Returns suggested tactics that close the goal.",
        "parameters": {
            "type": "object",
            "properties": {
                "tactic": {
                    "type": "string",
                    "enum": ["exact?", "apply?", "rw?"],
                    "description": "Which search tactic to run"
                },
                "file": {
                    "type": "string",
                    "description": "Relative path to the .lean file. Defaults to 'Scratch.lean'.",
                    "default": "Scratch.lean"
                },
                "line": {
                    "type": "integer",
                    "description": "1-based line number of the sorry to replace with the search tactic. If omitted, replaces the first sorry found."
                }
            },
            "required": ["tactic"],
            "additionalProperties": false
        }
    })
}

fn file_read_tool() -> Value {
    json!({
        "type": "function",
        "name": "file_read",
        "description": "Read a file from the session workspace. Returns the file contents with line numbers.",
        "parameters": {
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Relative path to the file within the workspace, e.g. 'Scratch.lean' or 'Helpers.lean'"
                }
            },
            "required": ["path"],
            "additionalProperties": false
        }
    })
}

fn file_write_tool() -> Value {
    json!({
        "type": "function",
        "name": "file_write",
        "description": "Write or create a file in the session workspace. Overwrites the file if it already exists.",
        "parameters": {
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Relative path for the file, e.g. 'Scratch.lean' or 'Helpers.lean'"
                },
                "content": {
                    "type": "string",
                    "description": "The full content to write to the file"
                }
            },
            "required": ["path", "content"],
            "additionalProperties": false
        }
    })
}

fn file_patch_tool() -> Value {
    json!({
        "type": "function",
        "name": "file_patch",
        "description": "Apply a surgical patch to an existing file in the workspace. Use the unified diff format with context lines, -old lines, and +new lines.",
        "parameters": {
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Relative path to the file to patch, e.g. 'Scratch.lean'"
                },
                "patch": {
                    "type": "string",
                    "description": "The patch in unified diff format:\n*** Begin Patch\n*** Update File: <path>\n@@ context\n context line\n-old line\n+new line\n context line\n*** End Patch"
                }
            },
            "required": ["path", "patch"],
            "additionalProperties": false
        }
    })
}

fn corpus_search_tool() -> Value {
    json!({
        "type": "function",
        "name": "corpus_search",
        "description": "Search the verified mathematical corpus for relevant lemmas, theorems, definitions, and previously failed attempts. Use this to find existing Mathlib results, look up exact lemma names, or check what proof approaches have been tried before. The corpus contains 190,000+ verified Mathlib declarations plus user-verified proofs.",
        "parameters": {
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query: a theorem name (e.g. 'Nat.Prime.dvd_factorial'), mathematical concept (e.g. 'prime divisor of factorial'), or Lean type signature fragment (e.g. 'Nat.Prime → dvd → le')"
                }
            },
            "required": ["query"],
            "additionalProperties": false
        }
    })
}

fn workspace_ls_tool() -> Value {
    json!({
        "type": "function",
        "name": "workspace_ls",
        "description": "List all files in the session workspace directory.",
        "parameters": {
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false
        }
    })
}
