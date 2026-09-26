//! Tests for the mechanical `/attach` command (#933).
//!
//! Pins the per-path verdicts (ok, directory, missing, confidential, hidden,
//! docs-allowlist) and the multi-path status lines. The confidential refusal
//! is the compiled `is_confidential` gate, so a key-shaped file must be
//! refused in code; hidden dotfiles and the docs-only allowlist are the
//! issue's other two hard gates.

use crate::channels::commands::attach_status_lines;
use std::fs;
use std::path::PathBuf;

fn tempdir_with(name: &str, contents: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join(name);
    fs::write(&file, contents).expect("write");
    (dir, file)
}

#[test]
fn md_file_is_attachable() {
    let (_dir, file) = tempdir_with("notes.md", "hello");

    let (lines, attached) = attach_status_lines(file.to_str().expect("utf8"));
    assert_eq!(attached, vec![file.to_string_lossy().to_string()]);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("attached"), "status line: {}", lines[0]);
}

#[test]
fn txt_file_under_docs_directory_is_attachable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let docs = dir.path().join("docs");
    fs::create_dir_all(&docs).expect("mkdir");
    let file = docs.join("runbook.txt");
    fs::write(&file, "steps").expect("write");

    let (lines, attached) = attach_status_lines(file.to_str().expect("utf8"));
    assert_eq!(attached, vec![file.to_string_lossy().to_string()]);
    assert!(lines[0].contains("attached"), "status line: {}", lines[0]);
}

#[test]
fn uppercase_extension_is_still_docs() {
    let (_dir, file) = tempdir_with("README.MD", "big letters");

    let (lines, attached) = attach_status_lines(file.to_str().expect("utf8"));
    assert_eq!(attached, vec![file.to_string_lossy().to_string()]);
    assert!(lines[0].contains("attached"), "status line: {}", lines[0]);
}

#[test]
fn non_docs_file_is_refused() {
    let (_dir, file) = tempdir_with("main.rs", "fn main() {}");

    let raw = file.to_str().expect("utf8");
    let (lines, attached) = attach_status_lines(raw);
    assert!(attached.is_empty(), "non-docs never attach: {attached:?}");
    assert_eq!(lines.len(), 1);
    assert!(
        lines[0].contains("docs-only"),
        "allowlist named in the refusal: {}",
        lines[0]
    );
}

#[test]
fn hidden_file_is_refused() {
    let (_dir, file) = tempdir_with(".hidden.md", "peekaboo");

    let raw = file.to_str().expect("utf8");
    let (lines, attached) = attach_status_lines(raw);
    assert!(attached.is_empty(), "hidden never attaches: {attached:?}");
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("hidden"), "hidden refusal: {}", lines[0]);
}

#[test]
fn leading_dot_slash_component_is_not_hidden() {
    // A user-typed RELATIVE path with an explicit ./ pins that the structural
    // CurDir component is not treated as a hidden segment. (Prepending ./ to
    // an absolute path would be a different case entirely: it turns the whole
    // thing CWD-relative, so NotFound is the correct verdict there.)
    let raw = "./README.md".to_string();

    let (lines, attached) = attach_status_lines(&raw);
    assert_eq!(attached, vec![raw.clone()]);
    assert!(lines[0].contains("attached"), "status line: {}", lines[0]);
}

#[test]
fn directory_is_rejected_with_architecture_hint() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sub = dir.path().join("subdir");
    fs::create_dir_all(&sub).expect("mkdir");

    let raw = sub.to_str().expect("utf8");
    let (lines, attached) = attach_status_lines(raw);
    assert!(
        attached.is_empty(),
        "directories never attach: {attached:?}"
    );
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("is a directory"), "line: {}", lines[0]);
    assert!(
        lines[0].contains(&format!("/architecture {raw}")),
        "hint names the alternative: {}",
        lines[0]
    );
}

#[test]
fn missing_path_reports_not_found() {
    let (lines, attached) = attach_status_lines("/definitely/not/here.md");
    assert!(attached.is_empty());
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("no such path"), "line: {}", lines[0]);
}

#[test]
fn confidential_file_is_refused_with_reason() {
    let (_dir, key) = tempdir_with("id_rsa", "PRIVATE KEY");

    let raw = key.to_str().expect("utf8");
    let (lines, attached) = attach_status_lines(raw);
    assert!(attached.is_empty(), "keys never attach: {attached:?}");
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("refused"), "line: {}", lines[0]);
    assert!(
        lines[0].contains("SSH private key"),
        "reason shown: {}",
        lines[0]
    );
}

#[test]
fn multi_path_invocation_reports_each_verdict_in_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let good = dir.path().join("good.md");
    fs::write(&good, "x").expect("write");
    let sub = dir.path().join("adir");
    fs::create_dir_all(&sub).expect("mkdir");
    let key = dir.path().join("id_ed25519");
    fs::write(&key, "PRIVATE KEY").expect("write");
    let src = dir.path().join("main.rs");
    fs::write(&src, "fn main() {}").expect("write");

    let args = format!(
        "{} {} {} {}",
        good.display(),
        sub.display(),
        key.display(),
        src.display()
    );
    let (lines, attached) = attach_status_lines(&args);
    assert_eq!(attached, vec![good.to_string_lossy().to_string()]);
    assert_eq!(lines.len(), 4, "one line per path: {lines:?}");
    assert!(lines[0].contains("attached"));
    assert!(lines[1].contains("is a directory"));
    assert!(lines[2].contains("refused"));
    assert!(lines[3].contains("docs-only"));
}

#[test]
fn empty_args_produce_no_lines_and_no_attachments() {
    let (lines, attached) = attach_status_lines("");
    assert!(lines.is_empty());
    assert!(attached.is_empty());
}
