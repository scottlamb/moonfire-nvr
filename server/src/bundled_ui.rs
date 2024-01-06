// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2023 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! UI bundled (compiled/linked) into the executable for single-file deployment.

use base::FastHashMap;
use http::{header, HeaderMap, HeaderValue};
use std::io::Read;
use std::sync::OnceLock;

use crate::body::{BoxedError, Chunk};

pub struct Ui(FastHashMap<&'static str, FileSet>);

/// A file as passed in from `build.rs`.
struct BuildFile {
    /// Path without any prefix (even `/`) for the root or any encoding suffix (`.gz`).
    bare_path: &'static str,
    data: &'static [u8],
    etag: &'static str,
    encoding: FileEncoding,
}

#[allow(unused)] // it's valid for a UI to have all uncompressed files or vice versa.
#[derive(Copy, Clone)]
enum FileEncoding {
    Uncompressed,
    Gzipped,
}

// `build.rs` fills in: `static FILES: [BuildFile; _] = [ ... ];`
include!(concat!(env!("OUT_DIR"), "/ui_files.rs"));

/// A file, ready to serve.
struct File {
    data: &'static [u8],
    etag: &'static str,
}

struct FileSet {
    uncompressed: File,
    gzipped: Option<File>,
}

impl Ui {
    pub fn get() -> &'static Self {
        UI.get_or_init(Self::init)
    }

    #[tracing::instrument]
    fn init() -> Self {
        Ui(FILES
            .iter()
            .map(|f| {
                let set = if matches!(f.encoding, FileEncoding::Gzipped) {
                    let mut uncompressed = Vec::new();
                    let mut d = flate2::read::GzDecoder::new(f.data);
                    d.read_to_end(&mut uncompressed)
                        .expect("bundled gzip files should be valid");

                    // TODO: use String::leak in rust 1.72+.
                    let etag = format!("{}.ungzipped", f.etag);
                    let etag = etag.into_bytes().leak();
                    let etag =
                        std::str::from_utf8(etag).expect("just-formatted str is valid utf-8");

                    FileSet {
                        uncompressed: File {
                            data: uncompressed.leak(),
                            etag,
                        },
                        gzipped: Some(File {
                            data: f.data,
                            etag: f.etag,
                        }),
                    }
                } else {
                    FileSet {
                        uncompressed: File {
                            data: f.data,
                            etag: f.etag,
                        },
                        gzipped: None,
                    }
                };
                (f.bare_path, set)
            })
            .collect())
    }

    pub fn lookup(
        &'static self,
        path: &str,
        hdrs: &HeaderMap<HeaderValue>,
        cache_control: &'static str,
        content_type: &'static str,
    ) -> Option<Entity> {
        let Some(set) = self.0.get(path) else {
            return None;
        };
        let auto_gzip;
        if let Some(ref gzipped) = set.gzipped {
            auto_gzip = true;
            if http_serve::should_gzip(hdrs) {
                return Some(Entity {
                    file: &gzipped,
                    auto_gzip,
                    is_gzipped: true,
                    cache_control,
                    content_type,
                });
            }
        } else {
            auto_gzip = false
        };
        Some(Entity {
            file: &set.uncompressed,
            auto_gzip,
            is_gzipped: false,
            cache_control,
            content_type,
        })
    }
}

static UI: OnceLock<Ui> = OnceLock::new();

#[derive(Copy, Clone)]
pub struct Entity {
    file: &'static File,
    auto_gzip: bool,
    is_gzipped: bool,
    cache_control: &'static str,
    content_type: &'static str,
}

impl http_serve::Entity for Entity {
    type Data = Chunk;
    type Error = BoxedError;

    fn len(&self) -> u64 {
        self.file
            .data
            .len()
            .try_into()
            .expect("usize should be convertible to u64")
    }

    fn get_range(
        &self,
        range: std::ops::Range<u64>,
    ) -> Box<dyn futures::Stream<Item = Result<Self::Data, Self::Error>> + Send + Sync> {
        let file = self.file;
        Box::new(futures::stream::once(async move {
            let r = usize::try_from(range.start)?..usize::try_from(range.end)?;
            let Some(data) = file.data.get(r) else {
                let len = file.data.len();
                return Err(format!("static file range {range:?} invalid (len {len:?})").into());
            };
            Ok(data.into())
        }))
    }

    fn add_headers(&self, hdrs: &mut http::HeaderMap) {
        if self.auto_gzip {
            hdrs.insert(header::VARY, HeaderValue::from_static("accept-encoding"));
        }
        if self.is_gzipped {
            hdrs.insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        }
        hdrs.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(self.cache_control),
        );
        hdrs.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(self.content_type),
        );
    }

    fn etag(&self) -> Option<http::HeaderValue> {
        Some(http::HeaderValue::from_static(self.file.etag))
    }

    fn last_modified(&self) -> Option<std::time::SystemTime> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_html_uncompressed() {
        let ui = Ui::get();
        let e = ui
            .lookup("index.html", &HeaderMap::new(), "public", "text/html")
            .unwrap();
        assert!(e.file.data.starts_with(b"<!doctype html"));
    }
}
