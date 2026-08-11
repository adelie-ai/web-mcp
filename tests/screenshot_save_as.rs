#![deny(warnings)]

//! Acceptance tests for `web_screenshot`'s `save_as` path (#20).
//!
//! `save_as` is external input: it arrives from the model, which takes
//! instruction from the pages it reads. The screenshot directory is therefore a
//! boundary, not a suggestion. Every escape route out of it gets its own named
//! test here, so a failing run names the route that opened.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use web_mcp::error::{WebError, WebMcpError};
use web_mcp::screenshot::ScreenshotDir;

/// A 1x1 PNG: the 8-byte signature, then an IHDR chunk declaring the size.
/// Only the header matters here - nothing decodes the image data.
const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, // signature
    0x00, 0x00, 0x00, 0x0d, // IHDR length
    0x49, 0x48, 0x44, 0x52, // "IHDR"
    0x00, 0x00, 0x00, 0x01, // width  = 1
    0x00, 0x00, 0x00, 0x01, // height = 1
    0x08, 0x06, 0x00, 0x00, 0x00, // bit depth, colour type, ...
];

/// A PNG whose IHDR declares 1280x720.
fn png_1280x720() -> Vec<u8> {
    let mut png = PNG_1X1.to_vec();
    png[16..20].copy_from_slice(&1280u32.to_be_bytes());
    png[20..24].copy_from_slice(&720u32.to_be_bytes());
    png
}

// ── Test scaffolding ─────────────────────────────────────────────────────────

static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

/// A directory under the system temp dir, removed when the test drops it.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "web-mcp-save-as-test-{}-{}",
            std::process::id(),
            seq
        ));
        fs::create_dir_all(&path).expect("create the scratch directory");
        Self { path }
    }

    fn join(&self, rel: &str) -> PathBuf {
        self.path.join(rel)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Assert that `outcome` was refused as a caller mistake, matching on the error
/// variant rather than on its message.
#[track_caller]
fn assert_refused<T: std::fmt::Debug>(outcome: Result<T, WebMcpError>, what: &str) {
    match outcome {
        Err(WebMcpError::Web(WebError::InvalidParameters(_))) => {}
        other => panic!("{what} should be refused as invalid parameters, got {other:?}"),
    }
}

/// True when `path` is inside `root` after both are fully resolved.
fn is_inside(root: &Path, path: &Path) -> bool {
    let root = fs::canonicalize(root).expect("canonicalize the root");
    let path = fs::canonicalize(path).expect("canonicalize the written file");
    path.starts_with(root)
}

// ── Accepted paths ───────────────────────────────────────────────────────────

#[test]
fn saves_a_png_under_the_screenshot_directory() {
    let scratch = Scratch::new();
    let root = scratch.join("shots");
    let dir = ScreenshotDir::new(&root);

    let saved = dir.save_png("example.png", &png_1280x720()).expect("save");

    let written = PathBuf::from(&saved.path);
    assert!(written.is_file(), "the file should exist at {}", saved.path);
    assert!(is_inside(&root, &written), "the file should be in the root");
    assert_eq!(fs::read(&written).expect("read back"), png_1280x720());
}

#[test]
fn reports_the_size_and_pixel_dimensions_of_what_it_wrote() {
    let scratch = Scratch::new();
    let dir = ScreenshotDir::new(scratch.join("shots"));
    let png = png_1280x720();

    let saved = dir.save_png("example.png", &png).expect("save");

    assert_eq!(saved.bytes, png.len() as u64);
    assert_eq!(saved.width, Some(1280));
    assert_eq!(saved.height, Some(720));
}

#[test]
fn creates_missing_subdirectories_under_the_root() {
    let scratch = Scratch::new();
    let root = scratch.join("shots");
    let dir = ScreenshotDir::new(&root);

    let saved = dir
        .save_png("reports/2026/pricing.png", PNG_1X1)
        .expect("save");

    let written = PathBuf::from(&saved.path);
    assert!(written.is_file(), "the nested file should exist");
    assert!(is_inside(&root, &written), "the file should be in the root");
}

#[test]
fn accepts_an_absolute_path_that_is_already_inside_the_root() {
    let scratch = Scratch::new();
    let root = scratch.join("shots");
    fs::create_dir_all(&root).expect("create the root");
    let dir = ScreenshotDir::new(&root);
    let absolute = root.join("inside.png");

    let saved = dir
        .save_png(&absolute.to_string_lossy(), PNG_1X1)
        .expect("an absolute path inside the root is accepted");

    assert!(PathBuf::from(&saved.path).is_file());
}

#[test]
fn writing_the_same_path_twice_leaves_one_file_with_the_later_content() {
    let scratch = Scratch::new();
    let dir = ScreenshotDir::new(scratch.join("shots"));

    let first = dir.save_png("same.png", PNG_1X1).expect("first save");
    let second = dir
        .save_png("same.png", &png_1280x720())
        .expect("second save");

    assert_eq!(
        first.path, second.path,
        "the same request yields the same path"
    );
    assert_eq!(
        fs::read(&second.path).expect("read back"),
        png_1280x720(),
        "the second write replaces the first"
    );
}

// ── Refused paths ────────────────────────────────────────────────────────────

#[test]
fn refuses_an_empty_path() {
    let scratch = Scratch::new();
    let dir = ScreenshotDir::new(scratch.join("shots"));

    assert_refused(dir.save_png("", PNG_1X1), "an empty save_as");
    assert_refused(dir.save_png("   ", PNG_1X1), "a blank save_as");
}

#[test]
fn refuses_a_parent_directory_escape() {
    let scratch = Scratch::new();
    let dir = ScreenshotDir::new(scratch.join("shots"));

    for escape in ["../escaped.png", "a/../../escaped.png", ".././escaped.png"] {
        assert_refused(dir.save_png(escape, PNG_1X1), escape);
    }
    assert!(
        !scratch.join("escaped.png").exists(),
        "nothing may be written beside the root"
    );
}

#[test]
fn refuses_an_absolute_path_outside_the_root() {
    let scratch = Scratch::new();
    let dir = ScreenshotDir::new(scratch.join("shots"));
    let outside = scratch.join("outside.png");

    assert_refused(
        dir.save_png(&outside.to_string_lossy(), PNG_1X1),
        "an absolute path outside the root",
    );
    assert_refused(
        dir.save_png("/etc/web-mcp-escaped.png", PNG_1X1),
        "an absolute system path",
    );
    assert!(!outside.exists(), "nothing may be written outside the root");
}

#[test]
fn refuses_a_path_that_is_not_a_png() {
    let scratch = Scratch::new();
    let dir = ScreenshotDir::new(scratch.join("shots"));

    for wrong in ["shot.sh", "shot", "shot.png.sh", ".bashrc"] {
        assert_refused(dir.save_png(wrong, PNG_1X1), wrong);
    }
}

#[test]
fn refuses_a_directory_as_the_target() {
    let scratch = Scratch::new();
    let dir = ScreenshotDir::new(scratch.join("shots"));

    assert_refused(dir.save_png("sub/", PNG_1X1), "a trailing-slash path");
}

#[cfg(unix)]
#[test]
fn refuses_a_symlinked_file_that_points_out_of_the_root() {
    let scratch = Scratch::new();
    let root = scratch.join("shots");
    fs::create_dir_all(&root).expect("create the root");
    let outside = scratch.join("outside.png");
    fs::write(&outside, b"original").expect("seed the outside file");
    std::os::unix::fs::symlink(&outside, root.join("link.png")).expect("symlink");

    let dir = ScreenshotDir::new(&root);

    assert_refused(dir.save_png("link.png", PNG_1X1), "a symlinked target");
    assert_eq!(
        fs::read(&outside).expect("read back"),
        b"original",
        "the file the symlink points at must be untouched"
    );
}

#[cfg(unix)]
#[test]
fn refuses_a_symlinked_directory_that_points_out_of_the_root() {
    let scratch = Scratch::new();
    let root = scratch.join("shots");
    fs::create_dir_all(&root).expect("create the root");
    let outside = scratch.join("outside-dir");
    fs::create_dir_all(&outside).expect("create the outside directory");
    std::os::unix::fs::symlink(&outside, root.join("escape")).expect("symlink");

    let dir = ScreenshotDir::new(&root);

    assert_refused(
        dir.save_png("escape/shot.png", PNG_1X1),
        "a symlinked directory",
    );
    assert!(
        !outside.join("shot.png").exists(),
        "nothing may be written through the symlinked directory"
    );
}

#[cfg(unix)]
#[test]
fn refuses_a_symlinked_directory_before_creating_anything_beneath_it() {
    // The nested case: a refusal must not create the directories on the way to
    // a path it is about to refuse, or an attacker-named path becomes an
    // attacker-directed mkdir outside the boundary.
    let scratch = Scratch::new();
    let root = scratch.join("shots");
    fs::create_dir_all(&root).expect("create the root");
    let outside = scratch.join("outside-dir");
    fs::create_dir_all(&outside).expect("create the outside directory");
    std::os::unix::fs::symlink(&outside, root.join("escape")).expect("symlink");

    let dir = ScreenshotDir::new(&root);

    assert_refused(
        dir.save_png("escape/deep/nested/shot.png", PNG_1X1),
        "a path through a symlinked directory",
    );
    assert!(
        !outside.join("deep").exists(),
        "no directory may be created outside the root"
    );
}

#[cfg(unix)]
#[test]
fn refuses_a_hard_link_that_points_at_a_file_outside_the_root() {
    // A hard link is a regular file to `symlink_metadata`, so the symlink rule
    // does not see it. Writing through one destroys a file outside the root.
    let scratch = Scratch::new();
    let root = scratch.join("shots");
    fs::create_dir_all(&root).expect("create the root");
    let victim = scratch.join("victim.txt");
    fs::write(&victim, b"original").expect("seed the victim file");
    fs::hard_link(&victim, root.join("hard.png")).expect("hard link");

    let dir = ScreenshotDir::new(&root);

    assert_refused(dir.save_png("hard.png", PNG_1X1), "a hard link");
    assert_eq!(
        fs::read(&victim).expect("read back"),
        b"original",
        "the file the hard link shares must be untouched"
    );
}

#[test]
fn refuses_a_path_deeper_than_the_directory_limit() {
    // Every component becomes a directory that is never reclaimed, so one call
    // must not be able to create an unbounded number of them.
    let scratch = Scratch::new();
    let root = scratch.join("shots");
    let dir = ScreenshotDir::new(&root);

    let deep = format!("{}shot.png", "d/".repeat(64));
    assert_refused(dir.save_png(&deep, PNG_1X1), "a very deep path");
    assert!(
        !root.join("d").exists(),
        "a refused path creates no directory at all"
    );
    // The limit is a limit, not a ban on grouping.
    dir.save_png("a/b/c/shot.png", PNG_1X1)
        .expect("ordinary grouping is still allowed");
}

#[test]
fn refuses_a_path_carrying_a_nul_byte() {
    // Without this the path passes the checks and fails at the write, so the
    // page is fetched first and the refusal arrives as an IO error instead.
    let scratch = Scratch::new();
    let dir = ScreenshotDir::new(scratch.join("shots"));

    assert_refused(
        dir.save_png("shot\0.png", PNG_1X1),
        "a path with a NUL byte",
    );
    assert!(
        dir.check("shot\0.png").is_err(),
        "and the early check agrees"
    );
}

#[test]
fn refuses_a_trailing_current_directory_component() {
    // "x.png/." names a directory just as "x.png/" does.
    let scratch = Scratch::new();
    let dir = ScreenshotDir::new(scratch.join("shots"));

    assert_refused(dir.save_png("shot.png/.", PNG_1X1), "a trailing '/.'");
}

#[test]
fn everything_the_early_check_accepts_is_accepted_by_the_write() {
    // `check` runs before the page is fetched and must not promise more than
    // the write delivers for reasons that are in the path itself.
    let scratch = Scratch::new();
    let dir = ScreenshotDir::new(scratch.join("shots"));

    for path in ["shot.png", "a/b.png", "Shot.PNG"] {
        dir.check(path).expect("check accepts it");
        dir.save_png(path, PNG_1X1).expect("and so does the write");
    }
}
