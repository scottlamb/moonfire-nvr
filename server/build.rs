// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2023 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

//! Build script to bundle UI files if `bundled-ui` Cargo feature is selected.

use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const UI_DIR: &str = "../ui/build";

fn ensure_link(original: &Path, link: &Path) {
    match std::fs::read_link(link) {
        Ok(dst) if dst == original => return,
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
/// `favicons/blah.ico` rather than `../../ui/build/favicons/blah.ico.gz`.
///
/// The best representation is gzipped if available, uncompressed otherwise.
type FileMap = fnv::FnvHashMap<String, File>;

fn stringify_files(files: &FileMap) -> Result<String, std::fmt::Error> {
    let mut buf = String::new();
    write!(buf, "const FILES: [BuildFile; {}] = [\n", files.len())?;
    for (bare_path, file) in files {
        let include_path = &file.include_path;
        let etag = file.etag.to_hex();
        let encoding = file.encoding.to_str();
        write!(buf, "    BuildFile {{ bare_path: {bare_path:?}, data: include_bytes!({include_path:?}), etag: {etag:?}, encoding: {encoding} }},\n")?;
    }
    write!(buf, "];\n")?;
    Ok(buf)
}

fn main() -> ExitCode {
    // Explicitly declare dependencies, so this doesn't re-run if other source files change.
    println!("cargo:rerun-if-changed=build.rs");

    // Nothing to do if the feature is off. cargo will re-run if features change.
    if !cfg!(feature = "bundled-ui") {
        return ExitCode::SUCCESS;
    }

    // If the feature is on, also re-run if the actual UI files change.
    println!("cargo:rerun-if-changed={UI_DIR}");

    let out_dir: PathBuf = std::env::var_os("OUT_DIR")
        .expect("cargo should set OUT_DIR")
        .into();

    let abs_ui_dir = std::fs::canonicalize(UI_DIR)
        .expect("ui dir should be accessible. Did you run `npm run build` first?");

    let mut files = FileMap::default();
    for entry in walkdir::WalkDir::new(&abs_ui_dir) {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                eprintln!(
                    "walkdir failed. Did you run `npm run build` first?\n\n\
                    caused by:\n{e}"
                );
                return ExitCode::FAILURE;
            }
        };
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
                if files.get(bare_path).is_some() {
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

    let files = stringify_files(&files).expect("write to String should succeed");
    let mut out_rs_path = std::path::PathBuf::new();
    out_rs_path.push(&out_dir);
    out_rs_path.push("ui_files.rs");
    std::fs::write(&out_rs_path, files).expect("writing ui_files.rs should succeed");

    let mut out_link_path = std::path::PathBuf::new();
    out_link_path.push(&out_dir);
    out_link_path.push("ui_files");
    ensure_link(&abs_ui_dir, &out_link_path);
    return ExitCode::SUCCESS;
}
