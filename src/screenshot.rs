#![deny(warnings)]

//! Where `web_screenshot` writes a captured PNG, and the boundary around it.
//!
//! `web_screenshot` can return the image inline, which puts a base64 blob in the
//! model's context, or write it to a file and return only the path and the
//! dimensions. The second form needs a path from the caller, and that caller is
//! a model acting on pages it has just read - so the path is external input.
//!
//! [`ScreenshotDir`] is the boundary. It owns one directory and resolves every
//! `save_as` inside it: no parent-directory hop, no absolute path elsewhere, no
//! link out - symbolic or hard - and no extension other than `.png`. Paths are
//! built with [`Path::join`], never by joining strings, and a refused path
//! creates nothing.
//!
//! What the boundary does not do, stated so nobody reads more into it: the
//! checks are path-based, so they are made against the filesystem as it is when
//! they run. Another process running as the same user can replace a checked
//! path between the check and the write. That process already has the user's
//! own write access, so it gains nothing it did not have; a caller of this
//! module gains nothing either. The boundary is against the `save_as` string,
//! not against a local attacker who is already this user.
//!
//! Where the directory itself comes from is the operator's decision, and it is
//! taken as given here - including through a symbolic link, which resolves and
//! then bounds everything inside it as normal.
//!
//! Non-goals: this module does not capture, decode, or re-encode an image. It
//! reads the PNG header for the dimensions it reports and writes the bytes it
//! is given; capture belongs to `operations::browser`.

use crate::error::{Result, WebError};
use serde::Serialize;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// The file extension `save_as` must carry. `web_screenshot` writes PNG and
/// nothing else, so any other extension is a caller mistake - and refusing it
/// keeps the tool from being steered into writing a shell profile or a script.
const REQUIRED_EXTENSION: &str = "png";

/// How many path components a `save_as` may have, the file name included - so
/// at most seven directory levels and a name. A screenshot needs a name and at
/// most a little grouping; a deeper path is a caller creating directories, not
/// filing a capture. Each level becomes a directory that is never reclaimed, so
/// the depth is bounded here.
const MAX_PATH_COMPONENTS: usize = 8;

/// The PNG signature, the first eight bytes of every PNG file.
const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];

/// What `web_screenshot` reports back after writing a file: enough for the
/// caller to open, share, or judge the size of the capture, and no image data.
#[derive(Debug, Clone, Serialize)]
pub struct SavedScreenshot {
    /// Absolute path of the written file.
    pub path: String,
    /// Size of the written file in bytes.
    pub bytes: u64,
    /// Image width in pixels, when the PNG header could be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    /// Image height in pixels, when the PNG header could be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
}

/// One directory that `web_screenshot` may write into, and the only route to it.
///
/// Construction is infallible and touches nothing: the directory is created on
/// the first successful save, so a server that never takes a screenshot never
/// makes one.
#[derive(Debug, Clone)]
pub struct ScreenshotDir {
    root: PathBuf,
}

impl ScreenshotDir {
    /// Wrap `root` as the one directory screenshots may be written into.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The directory this instance writes into, as configured.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Refuse a `save_as` this directory will not accept, without writing or
    /// creating anything.
    ///
    /// Why separate from [`save_png`](Self::save_png): a caller that names an
    /// unusable path should hear so before a page is fetched, not after. This
    /// runs the checks that read the path alone; the checks that must resolve
    /// symlinks still run at write time, because the filesystem can change
    /// between the two.
    pub fn check(&self, save_as: &str) -> Result<()> {
        self.relative_within_root(save_as).map(|_| ())
    }

    /// Write `png` to `save_as` inside this directory and describe the result.
    ///
    /// `save_as` is a path relative to the directory (`shot.png`,
    /// `reports/pricing.png`), or an absolute path that is already inside it.
    /// Missing parent directories are created. An existing file is replaced, so
    /// repeating the same call yields the same file and no extra side effect.
    ///
    /// Returns [`WebError::InvalidParameters`] when the path is refused, and an
    /// IO error when the write itself fails.
    pub fn save_png(&self, save_as: &str, png: &[u8]) -> Result<SavedScreenshot> {
        let target = self.resolve(save_as)?;
        fs::write(&target, png)?;
        let size = png_dimensions(png);
        Ok(SavedScreenshot {
            path: target.to_string_lossy().into_owned(),
            bytes: png.len() as u64,
            width: size.map(|(width, _)| width),
            height: size.map(|(_, height)| height),
        })
    }

    /// Resolve `save_as` to an absolute path inside this directory, creating any
    /// parent directories it needs.
    ///
    /// The checks run in two stages, and the order matters. The first stage
    /// reads the path and creates nothing, so a path refused there leaves no
    /// trace - not even the screenshot directory. The second stage walks the
    /// path one directory at a time, and creates a directory only below ground
    /// it has already proved is inside the root. That is what keeps a refusal
    /// from creating anything outside the boundary, and what catches a link
    /// planted inside the directory that points out of it.
    fn resolve(&self, save_as: &str) -> Result<PathBuf> {
        let relative = self.relative_within_root(save_as)?;
        let root = self.resolved_root()?;

        // Every component of `relative` is Normal - `relative_within_root`
        // refused anything else - and there is at least one, the file name.
        let components: Vec<&std::ffi::OsStr> =
            relative.components().map(Component::as_os_str).collect();
        let (name, directories) = components
            .split_last()
            .expect("relative_within_root returns at least one component");

        // Walk down from the root. `dir` is only ever a directory already
        // proved to resolve inside the root, so each step creates inside it.
        let mut dir = root.clone();
        for part in directories {
            let next = dir.join(part);
            if is_symlink(&next) {
                return Err(refuse(
                    save_as,
                    "a directory on the way to it is a symbolic link",
                ));
            }
            fs::create_dir_all(&next)?;
            dir = fs::canonicalize(&next)?;
            if !dir.starts_with(&root) {
                return Err(refuse(
                    save_as,
                    "its directory resolves outside the screenshot directory",
                ));
            }
        }

        // The file itself can be a link whose other end is elsewhere. Writing to
        // it writes through, so refuse both kinds: a symbolic link, and a file
        // that already carries more than one name. An existing directory is
        // checked first, so it is named as one rather than as a link - on some
        // filesystems a directory's link count is 2 even when it is empty.
        let target = dir.join(name);
        if is_symlink(&target) {
            return Err(refuse(save_as, "it is a symbolic link"));
        }
        if is_existing_directory(&target) {
            return Err(refuse(save_as, "a directory of that name already exists"));
        }
        if is_multiply_linked(&target) {
            return Err(refuse(
                save_as,
                "the file already there has another name elsewhere, so writing \
                 would change that file too; choose a different name",
            ));
        }
        Ok(target)
    }

    /// Create the screenshot directory if it is missing and return its resolved
    /// path, which is the boundary every other path is measured against.
    ///
    /// Resolving it once, here, is what makes the containment checks below
    /// meaningful: everything a caller names is compared against the directory
    /// as the filesystem actually reports it, not as it was written.
    fn resolved_root(&self) -> Result<PathBuf> {
        fs::create_dir_all(&self.root)?;
        Ok(fs::canonicalize(&self.root)?)
    }

    /// Read `save_as` and return the relative path it names inside the root.
    ///
    /// Refuses an empty path, any `..` component, a path that names a directory
    /// rather than a file, an extension other than `.png`, and an absolute path
    /// that is not already under the configured root. Creates nothing.
    fn relative_within_root(&self, save_as: &str) -> Result<PathBuf> {
        let trimmed = save_as.trim();
        if trimmed.is_empty() {
            return Err(refuse(save_as, "it is empty"));
        }
        // A NUL byte cannot appear in a path the operating system will accept.
        // Refusing it here rather than at the write keeps every path refusal a
        // parameter refusal, answered before the page is fetched.
        if trimmed.contains('\0') {
            return Err(refuse(save_as, "it contains a NUL byte"));
        }

        let given = Path::new(trimmed);
        // An absolute path is accepted only when it is already inside the
        // configured root; what is left after the root is an ordinary relative
        // path and goes through the same component checks as any other. The
        // root is matched as configured and as resolved, because a caller who
        // reads a resolved path back from an earlier reply should be able to
        // pass it straight in again.
        let candidate = if given.is_absolute() {
            self.strip_root(given).ok_or_else(|| {
                refuse(
                    save_as,
                    "an absolute path must be inside the screenshot directory",
                )
            })?
        } else {
            given.to_path_buf()
        };

        let mut relative = PathBuf::new();
        for component in candidate.components() {
            match component {
                Component::Normal(part) => relative.push(part),
                Component::CurDir => {}
                Component::ParentDir => {
                    return Err(refuse(save_as, "'..' is not allowed"));
                }
                Component::RootDir | Component::Prefix(_) => {
                    return Err(refuse(save_as, "it must be a path inside the directory"));
                }
            }
        }

        let file_name = relative
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .ok_or_else(|| refuse(save_as, "it does not name a file"))?;
        // A trailing separator and a trailing "/." both drop out of the
        // components above, leaving a path that resolves to a name the caller
        // did not finish writing. Requiring the text to end in the name it
        // resolves to catches every such form at once.
        if !trimmed.ends_with(file_name) {
            return Err(refuse(save_as, "it names a directory, not a file"));
        }
        if relative.components().count() > MAX_PATH_COMPONENTS {
            return Err(refuse(
                save_as,
                "it has more than 8 path components, counting the file name",
            ));
        }
        let is_png = relative
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case(REQUIRED_EXTENSION));
        if !is_png {
            return Err(refuse(save_as, "it must end in '.png'"));
        }

        Ok(relative)
    }

    /// Drop the root from the front of an absolute `given`, matching the root
    /// both as configured and as resolved. `None` when `given` is elsewhere.
    fn strip_root(&self, given: &Path) -> Option<PathBuf> {
        if let Ok(rest) = given.strip_prefix(&self.root) {
            return Some(rest.to_path_buf());
        }
        let resolved = fs::canonicalize(&self.root).ok()?;
        given.strip_prefix(resolved).ok().map(Path::to_path_buf)
    }
}

/// True when `path` exists and is a symbolic link. A path that does not exist,
/// or that cannot be read, is not one.
fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

/// True when `path` is an existing directory. Checked before the link count,
/// because a directory's link count is 2 or more on several filesystems and
/// would otherwise be reported as a link.
fn is_existing_directory(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir())
}

/// True when `path` exists and already carries more than one name, so writing
/// to it would write through to a file that may be outside the directory. A
/// hard link is a regular file to [`fs::symlink_metadata`], so the link count
/// is the only thing that tells the two apart.
///
/// This refuses a file web-mcp wrote itself once something else links to it -
/// a deduplication pass over a cache directory does exactly that. Refusing is
/// the safe direction, and the caller can use another name.
#[cfg(unix)]
fn is_multiply_linked(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    fs::symlink_metadata(path).is_ok_and(|meta| meta.nlink() > 1)
}

/// Link counts are not available here, so nothing is refused on that ground.
#[cfg(not(unix))]
fn is_multiply_linked(_path: &Path) -> bool {
    false
}

/// Build the refusal for a `save_as` the directory will not accept. The path is
/// echoed back so the model can correct it; `why` says which rule it broke.
fn refuse(save_as: &str, why: &str) -> crate::error::WebMcpError {
    WebError::InvalidParameters(format!("save_as '{save_as}' is refused: {why}")).into()
}

/// Read the pixel dimensions out of a PNG's IHDR header.
///
/// Returns `None` for anything that is not a PNG with a readable header. The
/// dimensions are reported to the caller, not relied on, so a header that
/// cannot be read costs the caller two fields and nothing else.
fn png_dimensions(png: &[u8]) -> Option<(u32, u32)> {
    // Signature (8), chunk length (4), chunk type (4), width (4), height (4).
    if png.len() < 24 || png[..8] != PNG_SIGNATURE || &png[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(png[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(png[20..24].try_into().ok()?);
    Some((width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ihdr(width: u32, height: u32) -> Vec<u8> {
        let mut png = Vec::from(PNG_SIGNATURE);
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&width.to_be_bytes());
        png.extend_from_slice(&height.to_be_bytes());
        png
    }

    #[test]
    fn png_dimensions_reads_the_ihdr_header() {
        assert_eq!(png_dimensions(&ihdr(1920, 1080)), Some((1920, 1080)));
    }

    #[test]
    fn png_dimensions_rejects_bytes_that_are_not_a_png() {
        assert_eq!(png_dimensions(b"not a png at all, truly"), None);
        assert_eq!(png_dimensions(&[]), None);
        // Right signature, truncated before the header is complete.
        assert_eq!(png_dimensions(&ihdr(1, 1)[..20]), None);
    }
}
