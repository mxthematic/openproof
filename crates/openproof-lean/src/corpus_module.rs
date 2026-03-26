//! Build and maintain the `OpenProof.Corpus` Lean module from corpus data.
//!
//! On app startup, the CLI fetches all user-verified proofs from cloud,
//! then calls `build_corpus_module` to write `OpenProof/Corpus.lean` and compile it.
//! Subsequent `import OpenProof.Corpus` in verification files resolves instantly.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// A corpus declaration to include in the Lean module.
pub struct CorpusDeclaration {
    pub label: String,
    pub statement: String,
    pub artifact_content: String,
}

/// Check whether the compiled corpus olean exists.
pub fn corpus_olean_exists(project_dir: &Path) -> bool {
    project_dir
        .join(".lake/build/lib/lean/OpenProof/Corpus.olean")
        .exists()
}

/// Path to the Corpus.lean source file.
pub fn corpus_lean_path(project_dir: &Path) -> PathBuf {
    project_dir.join("OpenProof/Corpus.lean")
}

/// Build `OpenProof/Corpus.lean` from a list of declarations and compile it.
///
/// Returns the number of declarations written, or 0 if no rebuild was needed.
pub fn build_corpus_module(
    project_dir: &Path,
    declarations: &[CorpusDeclaration],
) -> Result<usize> {
    if declarations.is_empty() {
        return Ok(0);
    }

    // Build Corpus.lean content, deduplicating by Lean declaration name.
    // Composite artifacts (theorem + its helpers) may overlap with standalone entries.
    let mut content = String::from("import Mathlib\n\n");
    let mut decl_count = 0usize;
    let mut seen_decl_names = std::collections::HashSet::new();

    for decl in declarations {
        let code = decl.artifact_content.trim();
        if code.is_empty() {
            continue;
        }
        let clean: String = code
            .lines()
            .filter(|l| {
                let t = l.trim();
                !t.starts_with("import ") && !t.starts_with("open ")
            })
            .collect::<Vec<_>>()
            .join("\n");
        let clean = clean.trim();
        if clean.is_empty() {
            continue;
        }
        // Check first declaration keyword
        let first_word = clean.split_whitespace().next().unwrap_or("");
        if !matches!(first_word, "theorem" | "lemma" | "def" | "noncomputable" | "instance" | "abbrev" | "set_option") {
            continue;
        }
        // Split into individual declaration blocks and dedup each
        let blocks: Vec<&str> = clean.split("\n\n").collect();
        let mut new_blocks = Vec::new();
        for block in &blocks {
            let block = block.trim();
            if block.is_empty() { continue; }
            if let Some(name) = extract_decl_name(block) {
                if seen_decl_names.contains(name) {
                    continue;
                }
                seen_decl_names.insert(name.to_string());
            }
            new_blocks.push(block);
        }
        if new_blocks.is_empty() {
            continue;
        }
        content.push_str(&format!("-- corpus: {}\n", decl.label));
        content.push_str(&new_blocks.join("\n\n"));
        content.push_str("\n\n");
        decl_count += 1;
    }

    if decl_count == 0 {
        return Ok(0);
    }

    // Check if content changed (hash comparison)
    let new_hash = hash_str(&content);
    let corpus_path = corpus_lean_path(project_dir);
    let existing_hash = std::fs::read_to_string(&corpus_path)
        .ok()
        .map(|s| hash_str(&s));

    if existing_hash == Some(new_hash) && corpus_olean_exists(project_dir) {
        return Ok(decl_count);
    }

    // Write new Corpus.lean
    std::fs::create_dir_all(corpus_path.parent().unwrap())
        .context("creating OpenProof dir")?;
    std::fs::write(&corpus_path, &content)
        .context("writing OpenProof/Corpus.lean")?;

    // Compile with lake build
    eprintln!(
        "[corpus-module] Building OpenProof.Corpus ({decl_count} declarations)..."
    );
    let output = std::process::Command::new("lake")
        .arg("build")
        .arg("OpenProof.Corpus")
        .current_dir(project_dir)
        .output()
        .context("running lake build OpenProof.Corpus")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!(
            "[corpus-module] Build failed: {}",
            &stderr[..stderr.len().min(500)]
        );
        return Ok(0);
    }

    eprintln!("[corpus-module] OpenProof.Corpus built ({decl_count} declarations)");
    Ok(decl_count)
}

/// Extract the declaration name from a Lean block (e.g. "theorem foo" -> "foo").
fn extract_decl_name(block: &str) -> Option<&str> {
    let first_line = block.lines().next()?;
    let prefixes = [
        "noncomputable def ",
        "theorem ",
        "lemma ",
        "def ",
        "instance ",
        "abbrev ",
    ];
    for prefix in &prefixes {
        if let Some(rest) = first_line.trim().strip_prefix(prefix) {
            return rest.split(|c: char| !c.is_alphanumeric() && c != '_').next();
        }
    }
    None
}

fn hash_str(s: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}
