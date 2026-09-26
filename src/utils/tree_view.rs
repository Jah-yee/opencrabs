//! Mechanical directory tree rendering for `/architecture` (#933).
//!
//! Zero LLM, zero tokens: a plain `std::fs` walk with a depth cap and an
//! exclusion list, rendered as indented text. Deterministic output (entries
//! sorted by lowercase name), bounded size, and honest about what it could
//! not read.

use std::fs;
use std::path::Path;

/// Default depth cap for `/architecture`, matching `tree -L 3`.
pub(crate) const TREE_DEFAULT_DEPTH: usize = 3;

/// Directory names never descended into (build artifacts, VCS internals,
/// dependency trees). Hidden entries (leading `.`) are always skipped too.
pub(crate) const TREE_EXCLUDED_DIRS: &[&str] =
    &[".git", "node_modules", "target", "dist", "vendor", ".venv"];

/// Hard cap on rendered entry lines so a huge tree cannot flood a chat
/// message. When the cap is hit the walk stops and a truncation marker is
/// appended.
pub(crate) const TREE_MAX_LINES: usize = 200;

/// Build the `/architecture` reply for `path` (or the working directory when
/// `None`). `Ok` carries the full user-facing tree text, `Err` a ready-to-send
/// refusal, so both surfaces (TUI system message, channel reply) render it
/// without reinterpreting anything.
pub(crate) fn architecture_tree(path: Option<&str>) -> Result<String, String> {
    let raw = path.unwrap_or(".").trim();
    let root = Path::new(raw);
    if !root.exists() {
        return Err(format!("architecture: no such path: {raw}"));
    }
    if !root.is_dir() {
        return Err(format!(
            "architecture: not a directory: {raw} (attach files with /attach instead)"
        ));
    }
    render_tree(root, TREE_DEFAULT_DEPTH)
}

/// Render `root` as an indented tree at most `max_depth` levels deep.
pub(crate) fn render_tree(root: &Path, max_depth: usize) -> Result<String, String> {
    let root_display = root.to_string_lossy().to_string();
    let mut lines = vec![format!("{root_display}/")];
    // Budget counts ENTRY lines only; the root header is always present.
    let mut budget = TREE_MAX_LINES;
    let mut truncated = false;
    walk_dir(
        root,
        "",
        0,
        max_depth,
        &mut lines,
        &mut budget,
        &mut truncated,
    );
    if truncated {
        lines.push(format!(
            "... output truncated at {TREE_MAX_LINES} entries (narrow the path or lower the depth)"
        ));
    }
    Ok(lines.join("\n"))
}

fn walk_dir(
    dir: &Path,
    prefix: &str,
    depth: usize,
    max_depth: usize,
    lines: &mut Vec<String>,
    budget: &mut usize,
    truncated: &mut bool,
) {
    if *truncated || depth >= max_depth {
        return;
    }
    let Ok(read_dir) = fs::read_dir(dir) else {
        // The parent already rendered this dir with a `[not readable]` mark;
        // nothing more to do here.
        return;
    };
    let mut entries: Vec<std::fs::DirEntry> = read_dir.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name().to_string_lossy().to_lowercase());
    for entry in entries {
        if *truncated {
            return;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || TREE_EXCLUDED_DIRS.contains(&name.as_str()) {
            continue;
        }
        let path = entry.path();
        let is_dir = path.is_dir();
        if *budget == 0 {
            *truncated = true;
            return;
        }
        *budget -= 1;
        if is_dir {
            // Probe readability now so the mark lands on the dir line itself.
            let readable = fs::read_dir(&path).is_ok();
            if readable {
                lines.push(format!("{prefix}{name}/"));
                walk_dir(
                    &path,
                    &format!("{prefix}  "),
                    depth + 1,
                    max_depth,
                    lines,
                    budget,
                    truncated,
                );
            } else {
                lines.push(format!("{prefix}{name}/ [not readable]"));
            }
        } else {
            lines.push(format!("{prefix}{name}"));
        }
    }
}
