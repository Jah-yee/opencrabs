//! Tests for the mechanical `/architecture` tree renderer (#933).
//!
//! Pins the depth cap, the exclusion list, hidden-entry skipping, empty-dir
//! rendering, unreadable-dir marking, and the output-size cap: the seams a
//! refactor could silently drift.

use crate::utils::tree_view::{TREE_MAX_LINES, architecture_tree, render_tree};
use std::fs;
use std::path::Path;

fn temp_tree(tag: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join(tag);
    fs::create_dir_all(&root).expect("mkdir root");
    dir
}

fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir parent");
    }
    fs::write(path, body).expect("write");
}

#[test]
fn depth_cap_stops_at_max_depth() {
    let dir = temp_tree("depth");
    let root = dir.path().join("depth");
    write(&root.join("l1/l2/l3/l4/deep.txt"), "x");
    write(&root.join("shallow.txt"), "x");

    let out = render_tree(&root, 3).expect("render");
    assert!(out.contains("l1/"), "level 1 present: {out}");
    assert!(out.contains("l2/"), "level 2 present: {out}");
    assert!(out.contains("l3/"), "level 3 present: {out}");
    assert!(!out.contains("l4/"), "level 4 must be cut: {out}");
    assert!(!out.contains("deep.txt"), "level 4 file must be cut: {out}");
    assert!(out.contains("shallow.txt"));
}

#[test]
fn excluded_dirs_and_hidden_entries_are_skipped() {
    let dir = temp_tree("excl");
    let root = dir.path().join("excl");
    write(&root.join(".git/HEAD"), "ref");
    write(&root.join("node_modules/pkg/index.js"), "x");
    write(&root.join("target/debug/bin"), "x");
    write(&root.join(".hidden"), "x");
    write(&root.join("src/main.rs"), "fn main() {}");

    let out = render_tree(&root, 3).expect("render");
    assert!(out.contains("src/"));
    assert!(out.contains("main.rs"));
    assert!(
        !out.contains(".git"),
        "hidden/vcs dir must be skipped: {out}"
    );
    assert!(
        !out.contains("node_modules"),
        "excluded dir must be skipped: {out}"
    );
    assert!(
        !out.contains("target"),
        "excluded dir must be skipped: {out}"
    );
    assert!(
        !out.contains(".hidden"),
        "hidden file must be skipped: {out}"
    );
}

#[test]
fn empty_dir_renders_bare_and_missing_path_errors() {
    let dir = temp_tree("empty");
    let root = dir.path().join("empty");
    fs::create_dir_all(root.join("nothing")).expect("mkdir");

    let out = render_tree(&root, 3).expect("render");
    assert!(out.contains("nothing/"), "empty dir renders bare: {out}");

    // The missing-root refusal is architecture_tree's contract: it owns
    // validation and formats the user-facing error. render_tree assumes a
    // validated, existing root and renders what it is given.
    let missing = root.join("nope");
    let err = architecture_tree(Some(missing.to_string_lossy().as_ref()))
        .expect_err("missing root errors");
    assert!(err.contains("no such path"), "error text: {err}");
}

#[test]
fn unreadable_dir_is_marked_not_read() {
    let dir = temp_tree("perm");
    let root = dir.path().join("perm");
    let secret = root.join("secret");
    fs::create_dir_all(&secret).expect("mkdir secret");
    write(&secret.join("inner.txt"), "x");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o000)).expect("chmod");
        // Running as root defeats permission bits; skip rather than flake.
        if fs::read_dir(&secret).is_ok() {
            fs::set_permissions(&secret, fs::Permissions::from_mode(0o755)).expect("restore");
            return;
        }
        let out = render_tree(&root, 3).expect("render");
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o755)).expect("restore");
        assert!(
            out.contains("secret/ [not readable]"),
            "unreadable dir must be marked: {out}"
        );
        assert!(!out.contains("inner.txt"), "content must not leak: {out}");
    }
}

#[test]
fn output_is_capped_at_max_lines() {
    let dir = temp_tree("cap");
    let root = dir.path().join("cap");
    for i in 0..(TREE_MAX_LINES + 50) {
        write(&root.join(format!("file_{i:04}.txt")), "x");
    }
    let out = render_tree(&root, 3).expect("render");
    let line_count = out.lines().count();
    assert!(
        line_count <= TREE_MAX_LINES + 2,
        "rendered {line_count} lines, cap is {TREE_MAX_LINES} (+root+marker)"
    );
    assert!(out.contains("output truncated"), "marker present: {out}");
}

#[test]
fn entries_render_in_deterministic_lowercase_order() {
    let dir = temp_tree("order");
    let root = dir.path().join("order");
    write(&root.join("Zebra.txt"), "x");
    write(&root.join("apple.txt"), "x");
    write(&root.join("Banana.txt"), "x");

    let out = render_tree(&root, 3).expect("render");
    let apple = out.find("apple.txt").expect("apple");
    let banana = out.find("Banana.txt").expect("banana");
    let zebra = out.find("Zebra.txt").expect("zebra");
    assert!(apple < banana && banana < zebra, "sorted order: {out}");
}
