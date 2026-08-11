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
//! symlink out, and no extension other than `.png`. Paths are built with
//! [`Path::join`], never by joining strings.
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
    /// reads the path and creates nothing, so a refused path leaves no trace -
    /// not even the screenshot directory. The second stage then resolves
    /// symlinks and re-checks containment, which is what catches a link planted
    /// inside the directory that points out of it.
    fn resolve(&self, save_as: &str) -> Result<PathBuf> {
        let relative = self.relative_within_root(save_as)?;

        // Filesystem stage. Create the directory, then resolve it: a caller's
        // path is only ever compared against the fully-resolved root.
        fs::create_dir_all(&self.root)?;
        let root = fs::canonicalize(&self.root)?;
        let target = root.join(&relative);

        let parent = target
            .parent()
            .ok_or_else(|| refuse(save_as, "it has no parent directory"))?;
        fs::create_dir_all(parent)?;
        let parent = fs::canonicalize(parent)?;
        if !parent.starts_with(&root) {
            return Err(refuse(
                save_as,
                "its directory resolves outside the screenshot directory",
            ));
        }

        // A path that resolves inside the directory can still be a symlink whose
        // target is not. Replacing a symlink writes through it, so refuse one.
        if let Ok(meta) = fs::symlink_metadata(&target)
            && meta.file_type().is_symlink()
        {
            return Err(refuse(save_as, "it is a symbolic link"));
        }

        let name = target
            .file_name()
            .ok_or_else(|| refuse(save_as, "it does not name a file"))?;
        Ok(parent.join(name))
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

        // A trailing separator drops out of `components()`, so compare the last
        // component against the raw text to catch a path that names a directory.
        if trimmed.ends_with('/') || trimmed.ends_with(std::path::MAIN_SEPARATOR) {
            return Err(refuse(save_as, "it names a directory, not a file"));
        }
        if relative.file_name().is_none() {
            return Err(refuse(save_as, "it does not name a file"));
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
