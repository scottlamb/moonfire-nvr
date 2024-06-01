// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2023 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

//! Build script to bundle UI files if `bundled-ui` Cargo feature is selected.

use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

const UI_DIST_DIR_ENV_VAR: &str = "UI_DIST_DIR";
const DEFAULT_UI_DIST_DIR: &str = "../ui/dist";

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

fn ensure_link(original: &Path, link: &Path) {
    match std::fs::read_link(link) {
        Ok(dst) if dst == original => return,
        Ok(_) => std::fs::remove_file(link).expect("removing stale symlink should succeed"),
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            panic!("couldn't create link {link:?} to original path {original:?}: {e}")
        }
        _ => {}
    }
    std::os::unix::fs::symlink(original, link).expect("symlink creation should succeed");
}

struct File {
    /// Path with `ui_files/` prefix and the encoding suffix; suitable for
    /// passing to `include_bytes!` in the expanded code.
    ///
    /// E.g. `ui_files/index.html.gz`.
    include_path: String,
    encoding: FileEncoding,
    etag: blake3::Hash,
}

#[derive(Copy, Clone)]
enum FileEncoding {
    Uncompressed,
    Gzipped,
}

impl FileEncoding {
    fn to_str(self) -> &'static str {
        match self {
            Self::Uncompressed => "FileEncoding::Uncompressed",
            Self::Gzipped => "FileEncoding::Gzipped",
        }
    }
}

/// Map of "bare path" to the best representation.
///
/// A "bare path" has no prefix for the root and no suffix for encoding, e.g.
/// `favicons/blah.ico` rather than `../../ui/dist/favicons/blah.ico.gz`.
///
/// The best representation is gzipped if available, uncompressed otherwise.
type FileMap = std::collections::HashMap<String, File, ahash::RandomState>;

fn stringify_files(files: &FileMap) -> Result<String, std::fmt::Error> {
    let mut buf = String::new();
    writeln!(buf, "const FILES: [BuildFile; {}] = [", files.len())?;
    for (bare_path, file) in files {
        let include_path = &file.include_path;
        let etag = file.etag.to_hex();
        let encoding = file.encoding.to_str();
        writeln!(buf, "    BuildFile {{ bare_path: {bare_path:?}, data: include_bytes!({include_path:?}), etag: {etag:?}, encoding: {encoding} }},")?;
    }
    writeln!(buf, "];")?;
    Ok(buf)
}

fn handle_bundled_ui() -> Result<(), BoxError> {
    // Nothing to do if the feature is off. cargo will re-run if features change.
    if !cfg!(feature = "bundled-ui") {
        return Ok(());
    }

    let ui_dir =
        std::env::var(UI_DIST_DIR_ENV_VAR).unwrap_or_else(|_| DEFAULT_UI_DIST_DIR.to_owned());

    // If the feature is on, also re-run if the actual UI files change.
    println!("cargo:rerun-if-env-changed={UI_DIST_DIR_ENV_VAR}");
    println!("cargo:rerun-if-changed={ui_dir}");

    let out_dir: PathBuf = std::env::var_os("OUT_DIR")
        .expect("cargo should set OUT_DIR")
        .into();

    let abs_ui_dir = std::fs::canonicalize(&ui_dir).map_err(|e| format!("ui dir {ui_dir:?} should be accessible. Did you run `npm run build` first?\n\ncaused by:\n{e}"))?;

    let mut files = FileMap::default();
    for entry in walkdir::WalkDir::new(&abs_ui_dir) {
        let entry = entry.map_err(|e| {
            format!("walkdir failed. Did you run `npm run build` first?\n\ncaused by:\n{e}")
        })?;
        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry
            .path()
            .strip_prefix(&abs_ui_dir)
            .expect("walkdir should return root-prefixed entries");
        let path = path.to_str().expect("ui file paths should be valid UTF-8");
        let (bare_path, encoding);
        match path.strip_suffix(".gz") {
            Some(p) => {
                bare_path = p;
                encoding = FileEncoding::Gzipped;
            }
            None => {
                bare_path = path;
                encoding = FileEncoding::Uncompressed;
                if files.contains_key(bare_path) {
                    continue; // don't replace with suboptimal encoding.
                }
            }
        }

        let contents = std::fs::read(entry.path()).expect("ui files should be readable");
        let etag = blake3::hash(&contents);
        let include_path = format!("ui_files/{path}");
        files.insert(
            bare_path.to_owned(),
            File {
                include_path,
                encoding,
                etag,
            },
        );
    }

    if !files.contains_key("index.html") {
        return Err(format!(
            "No `index.html` within {ui_dir:?}. Did you run `npm run build` first?"
        )
        .into());
    }

    let files = stringify_files(&files).expect("write to String should succeed");
    let mut out_rs_path = std::path::PathBuf::new();
    out_rs_path.push(&out_dir);
    out_rs_path.push("ui_files.rs");
    std::fs::write(&out_rs_path, files).expect("writing ui_files.rs should succeed");

    let mut out_link_path = std::path::PathBuf::new();
    out_link_path.push(&out_dir);
    out_link_path.push("ui_files");
    ensure_link(&abs_ui_dir, &out_link_path);
    Ok(())
}

/// Returns one-line `stdout` from a `git` command; `args` are simply space-separated (no escapes).
fn git_oneline_output(args: &str) -> Result<String, BoxError> {
    static HELP_TEXT: &str =
        "If you are building from a release archive or without the `git` CLI available, \n\
        try again with the `VERSION` environment variable set";

    // `output()` returns `Err` e.g. if `git` was not found.
    let mut output = Command::new("git")
        .args(args.split(' '))
        .output()
        .map_err(|e| format!("`git {args}` failed: {e}\n\n{HELP_TEXT}"))?;

    // `status` is non-success if `git` launched and then failed.
    if !output.status.success() {
        let status = output.status;
        let stderr = output.stderr.escape_ascii();
        return Err(format!("`git {args}` failed with {status}: {stderr}\n\n{HELP_TEXT}").into());
    }
    if output.stdout.pop() != Some(b'\n') {
        return Err(format!("`git {args}` stdout should end with newline").into());
    }
    if output.stdout.contains(&b'\n') {
        return Err(format!("`git {args}` stdout should be single line").into());
    }
    Ok(String::from_utf8(output.stdout)
        .map_err(|_| format!("`git {args}` stdout should be valid UTF-8"))?)
}

fn handle_version() -> Result<(), BoxError> {
    println!("cargo:rerun-if-env-changed=VERSION");
    if std::env::var("VERSION").is_ok() {
        return Ok(());
    }

    // Get version from `git describe`. Inspired by the `git-version` crate.
    // We don't use that directly because `cross`'s default docker image doesn't install `git`,
    // and thus we need the environment variable pass-through above.

    // Avoid reruns when the output doesn't meaningfully change. I don't think this is quite right:
    // it won't recognize toggling between `-dirty` and not. But it'll do.
    let dir = git_oneline_output("rev-parse --git-dir")?;
    println!("cargo:rerun-if-changed={dir}/logs/HEAD");
    println!("cargo:rerun-if-changed={dir}/index");

    // Plumb the version through.
    let version = git_oneline_output("describe --always --dirty")?;
    println!("cargo:rustc-env=VERSION={version}");

    Ok(())
}

fn main() -> Result<(), BoxError> {
    // Explicitly declare dependencies, so this doesn't re-run if other source files change.
    println!("cargo:rerun-if-changed=build.rs");
    handle_bundled_ui()?;
    handle_version()?;
    Ok(())
}
