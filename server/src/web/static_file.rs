// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Static file serving.

use std::sync::Arc;

use base::{bail, err, ErrorKind, ResultExt};
use http::{header, HeaderValue, Request};
use http_serve::dir::FsDir;
use tracing::warn;

use crate::cmds::run::config::UiDir;

use super::{ResponseResult, Service};

pub enum Ui {
    None,
    FromFilesystem(Arc<FsDir>),
    #[cfg(feature = "bundled-ui")]
    Bundled(&'static crate::bundled_ui::Ui),
}

impl Ui {
    pub fn from(cfg: &UiDir) -> Self {
        match cfg {
            UiDir::FromFilesystem(d) => match FsDir::builder().for_path(d) {
                Err(err) => {
                    warn!(
                        %err,
                        "unable to load ui dir {}; will serve no static files",
                        d.display(),
                    );
                    Self::None
                }
                Ok(d) => Self::FromFilesystem(d),
            },
            #[cfg(feature = "bundled-ui")]
            UiDir::Bundled(_) => Self::Bundled(crate::bundled_ui::Ui::get()),
            #[cfg(not(feature = "bundled-ui"))]
            UiDir::Bundled(_) => {
                warn!("server compiled without bundled ui; will serve not static files");
                Self::None
            }
        }
    }

    async fn serve(
        &self,
        path: &str,
        req: &Request<hyper::Body>,
        cache_control: &'static str,
        content_type: &'static str,
    ) -> ResponseResult {
        match self {
            Ui::None => bail!(
                NotFound,
                msg("ui not configured or missing; no static files available")
            ),
            Ui::FromFilesystem(d) => {
                let node = d.clone().get(path, req.headers()).await.map_err(|e| {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        err!(NotFound, msg("static file not found"))
                    } else {
                        err!(Internal, source(e))
                    }
                })?;
                let mut hdrs = http::HeaderMap::new();
                node.add_encoding_headers(&mut hdrs);
                hdrs.insert(
                    header::CACHE_CONTROL,
                    HeaderValue::from_static(cache_control),
                );
                hdrs.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
                let e = node.into_file_entity(hdrs).err_kind(ErrorKind::Internal)?;
                Ok(http_serve::serve(e, &req))
            }
            #[cfg(feature = "bundled-ui")]
            Ui::Bundled(ui) => {
                let Some(e) = ui.lookup(path, req.headers(), cache_control, content_type) else {
                    bail!(NotFound, msg("static file not found"));
                };
                Ok(http_serve::serve(e, &req))
            }
        }
    }
}

impl Service {
    /// Serves a static file if possible.
    pub(super) async fn static_file(&self, req: Request<hyper::Body>) -> ResponseResult {
        let Some(static_req) = StaticFileRequest::parse(req.uri().path()) else {
            bail!(NotFound, msg("static file not found"));
        };
        let cache_control = if static_req.immutable {
            // https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Cache-Control#Caching_static_assets
            "public, max-age=604800, immutable"
        } else {
            "public"
        };
        self.ui
            .serve(static_req.path, &req, cache_control, static_req.mime)
            .await
    }
}

#[derive(Debug, Eq, PartialEq)]
struct StaticFileRequest<'a> {
    path: &'a str,
    immutable: bool,
    mime: &'static str,
}

impl<'a> StaticFileRequest<'a> {
    fn parse(path: &'a str) -> Option<Self> {
        if !path.starts_with('/') || path == "/index.html" {
            return None;
        }

        let (path, immutable) = match &path[1..] {
            // These well-known URLs don't have content hashes in them, and
            // thus aren't immutable.
            "" => ("index.html", false),
            "robots.txt" => ("robots.txt", false),
            "site.webmanifest" => ("site.webmanifest", false),

            // Everything else is assumed to contain a hash and be immutable.
            p => (p, true),
        };

        let last_dot = match path.rfind('.') {
            None => return None,
            Some(d) => d,
        };
        let ext = &path[last_dot + 1..];
        let mime = match ext {
            "css" => "text/css",
            "html" => "text/html",
            "ico" => "image/x-icon",
            "js" | "map" => "text/javascript",
            "json" => "application/json",
            "png" => "image/png",
            "svg" => "image/svg+xml",
            "txt" => "text/plain",
            "webmanifest" => "application/manifest+json",
            "woff2" => "font/woff2",
            _ => return None,
        };

        Some(StaticFileRequest {
            path,
            immutable,
            mime,
        })
    }
}

#[cfg(test)]
mod tests {
    use db::testutil;

    use super::StaticFileRequest;

    #[test]
    fn static_file() {
        testutil::init();
        let r = StaticFileRequest::parse("/jquery-ui.b6d3d46c828800e78499.js").unwrap();
        assert_eq!(
            r,
            StaticFileRequest {
                path: "jquery-ui.b6d3d46c828800e78499.js",
                mime: "text/javascript",
                immutable: true,
            }
        );

        let r = StaticFileRequest::parse("/").unwrap();
        assert_eq!(
            r,
            StaticFileRequest {
                path: "index.html",
                mime: "text/html",
                immutable: false,
            }
        );
    }
}
