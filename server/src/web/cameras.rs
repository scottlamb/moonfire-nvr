// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2024 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Camera management: `/api/cameras/*`.

use base::{bail, err, Error, ErrorKind, ResultExt};
use http::{Method, Request, StatusCode};
use std::str::FromStr;
use uuid::Uuid;

use crate::json::{
    self, CameraWithId, GetCamerasResponse, PostCamerasResponse, TestCameraResponse,
};
use crate::stream::{self, Opener};

use super::{
    into_json_body, parse_json_body, plain_response, require_csrf_if_session, serve_json, Caller,
    ResponseResult, Service,
};

impl Service {
    pub(super) async fn cameras(
        &self,
        req: Request<hyper::body::Incoming>,
        caller: Caller,
    ) -> ResponseResult {
        match *req.method() {
            Method::GET | Method::HEAD => self.get_cameras(req, caller).await,
            Method::POST => self.post_cameras(req, caller).await,
            _ => Ok(plain_response(
                StatusCode::METHOD_NOT_ALLOWED,
                "GET, HEAD, or POST expected",
            )),
        }
    }

    async fn get_cameras(
        &self,
        req: Request<hyper::body::Incoming>,
        caller: Caller,
    ) -> ResponseResult {
        if !caller.permissions.read_camera_configs {
            bail!(
                Unauthenticated,
                msg("must have read_camera_configs permission")
            );
        }
        let l = self.db.lock();
        let cameras = l
            .cameras_by_id()
            .iter()
            .map(|(&id, camera)| -> Result<CameraWithId<'_>, Error> {
                Ok(CameraWithId {
                    id,
                    uuid: camera.uuid,
                    camera: json::Camera::wrap(camera, &l, false, true)?,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        serve_json(&req, &GetCamerasResponse { cameras })
    }

    async fn post_cameras(
        &self,
        req: Request<hyper::body::Incoming>,
        caller: Caller,
    ) -> ResponseResult {
        if !caller.permissions.admin_cameras {
            bail!(Unauthenticated, msg("must have admin_cameras permission"));
        }
        let (parts, b) = into_json_body(req).await?;
        let mut r: json::PostCameras = parse_json_body(&b)?;
        require_csrf_if_session(&caller, r.csrf)?;

        let short_name = r
            .camera
            .short_name
            .take()
            .ok_or_else(|| err!(InvalidArgument, msg("short_name must be specified")))?;

        let mut change = db::CameraChange {
            short_name: short_name.to_owned(),
            config: db::json::CameraConfig::default(),
            streams: Default::default(),
        };

        self.apply_camera_subset_to_change(&mut change, r.camera)?;

        let mut l = self.db.lock();
        let camera_id = l.add_camera(change)?;
        let camera = l.cameras_by_id().get(&camera_id).unwrap();
        serve_json(
            &parts,
            &PostCamerasResponse {
                id: camera_id,
                uuid: camera.uuid,
            },
        )
    }

    pub(super) async fn camera(
        &self,
        req: Request<hyper::body::Incoming>,
        caller: Caller,
        uuid: Uuid,
    ) -> ResponseResult {
        match *req.method() {
            Method::GET | Method::HEAD => self.get_camera(req, caller, uuid).await,
            Method::PATCH => self.patch_camera(req, caller, uuid).await,
            Method::DELETE => self.delete_camera(req, caller, uuid).await,
            _ => Ok(plain_response(
                StatusCode::METHOD_NOT_ALLOWED,
                "GET, HEAD, PATCH, or DELETE expected",
            )),
        }
    }

    async fn get_camera(
        &self,
        req: Request<hyper::body::Incoming>,
        caller: Caller,
        uuid: Uuid,
    ) -> ResponseResult {
        if !caller.permissions.read_camera_configs {
            bail!(
                Unauthenticated,
                msg("must have read_camera_configs permission")
            );
        }
        let db = self.db.lock();
        let camera = db
            .get_camera(uuid)
            .ok_or_else(|| err!(NotFound, msg("no such camera {uuid}")))?;
        serve_json(
            &req,
            &json::Camera::wrap(camera, &db, true, true).err_kind(ErrorKind::Internal)?,
        )
    }

    async fn patch_camera(
        &self,
        req: Request<hyper::body::Incoming>,
        caller: Caller,
        uuid: Uuid,
    ) -> ResponseResult {
        if !caller.permissions.admin_cameras {
            bail!(Unauthenticated, msg("must have admin_cameras permission"));
        }
        let (_parts, b) = into_json_body(req).await?;
        let r: json::PatchCamera = parse_json_body(&b)?;
        require_csrf_if_session(&caller, r.csrf)?;

        let mut l = self.db.lock();
        let camera_id = l
            .get_camera(uuid)
            .map(|c| c.id)
            .ok_or_else(|| err!(NotFound, msg("no such camera {uuid}")))?;

        let mut change = l.null_camera_change(camera_id)?;

        // Apply precondition checks if provided
        if let Some(precondition) = r.precondition {
            self.check_camera_precondition(&l, camera_id, precondition)?;
        }

        // Apply updates if provided
        if let Some(update) = r.update {
            self.apply_camera_subset_to_change(&mut change, update)?;
        }

        l.update_camera(camera_id, change)?;
        Ok(plain_response(StatusCode::NO_CONTENT, &b""[..]))
    }

    async fn delete_camera(
        &self,
        req: Request<hyper::body::Incoming>,
        caller: Caller,
        uuid: Uuid,
    ) -> ResponseResult {
        if !caller.permissions.admin_cameras {
            bail!(Unauthenticated, msg("must have admin_cameras permission"));
        }
        let (_parts, b) = into_json_body(req).await?;
        let r: json::DeleteCamera = parse_json_body(&b)?;
        require_csrf_if_session(&caller, r.csrf)?;

        let mut l = self.db.lock();
        let camera_id = l
            .get_camera(uuid)
            .map(|c| c.id)
            .ok_or_else(|| err!(NotFound, msg("no such camera {uuid}")))?;

        l.delete_camera(camera_id)?;
        Ok(plain_response(StatusCode::NO_CONTENT, &b""[..]))
    }

    pub(super) async fn camera_test(
        &self,
        req: Request<hyper::body::Incoming>,
        caller: Caller,
        uuid: Uuid,
    ) -> ResponseResult {
        if !caller.permissions.admin_cameras {
            bail!(Unauthenticated, msg("must have admin_cameras permission"));
        }
        if *req.method() != Method::POST {
            return Ok(plain_response(
                StatusCode::METHOD_NOT_ALLOWED,
                "POST expected",
            ));
        }

        let (parts, b) = into_json_body(req).await?;
        let r: json::TestCamera = parse_json_body(&b)?;
        require_csrf_if_session(&caller, r.csrf)?;

        let (url, camera_config) = {
            let db = self.db.lock();
            let camera = db
                .get_camera(uuid)
                .ok_or_else(|| err!(NotFound, msg("no such camera {uuid}")))?;

            let stream_id = camera.streams[r.stream_type.index()];
            if stream_id.is_none() {
                bail!(
                    InvalidArgument,
                    msg("camera has no {} stream configured", r.stream_type.as_str())
                );
            }
            let stream_id = stream_id.unwrap();

            let stream = db
                .streams_by_id()
                .get(&stream_id)
                .ok_or_else(|| err!(Internal, msg("missing stream {stream_id}")))?;

            let url = stream
                .config
                .url
                .as_ref()
                .ok_or_else(|| err!(InvalidArgument, msg("stream has no URL configured")))?
                .clone();

            (url, camera.config.clone())
        };

        let result = self.test_camera_stream(url, &camera_config).await;

        match result {
            Ok(message) => serve_json(
                &parts,
                &TestCameraResponse {
                    success: true,
                    message,
                },
            ),
            Err(e) => serve_json(
                &parts,
                &TestCameraResponse {
                    success: false,
                    message: e.to_string(),
                },
            ),
        }
    }

    /// Apply a CameraSubset to a CameraChange, handling validation and URL parsing.
    fn apply_camera_subset_to_change(
        &self,
        change: &mut db::CameraChange,
        mut subset: json::CameraSubset,
    ) -> Result<(), Error> {
        if let Some(short_name) = subset.short_name.take() {
            change.short_name = short_name.to_owned();
        }

        if let Some(description) = subset.description.take() {
            change.config.description = description.to_owned();
        }

        if let Some(onvif_base_url) = subset.onvif_base_url.take() {
            change.config.onvif_base_url = if onvif_base_url.is_empty() {
                None
            } else {
                Some(self.parse_url("onvif_base_url", onvif_base_url, &["http", "https"])?)
            };
        }

        if let Some(username) = subset.username.take() {
            change.config.username = username.to_owned();
        }

        if let Some(password) = subset.password.take() {
            change.config.password = password.to_owned();
        }

        if let Some(streams) = subset.streams.take() {
            for (i, stream_subset) in streams.into_iter().enumerate() {
                let stream_type = db::StreamType::from_index(i).unwrap();
                self.apply_stream_subset_to_change(
                    &mut change.streams[i],
                    stream_subset,
                    stream_type,
                )?;
            }
        }

        // Safety valve in case something is added to CameraSubset and forgotten here.
        if subset != Default::default() {
            bail!(
                Unimplemented,
                msg("camera updates not supported: {subset:#?}"),
            );
        }

        Ok(())
    }

    /// Apply a StreamSubset to a StreamChange, handling validation.
    fn apply_stream_subset_to_change(
        &self,
        change: &mut db::StreamChange,
        mut subset: json::StreamSubset,
        stream_type: db::StreamType,
    ) -> Result<(), Error> {
        if let Some(url) = subset.url.take() {
            change.config.url = if url.is_empty() {
                None
            } else {
                Some(self.parse_stream_url(stream_type, url)?)
            };
        }

        if let Some(record) = subset.record.take() {
            change.config.mode = if record {
                db::json::STREAM_MODE_RECORD.to_owned()
            } else {
                String::new()
            };
        }

        if let Some(flush_if_sec) = subset.flush_if_sec.take() {
            if flush_if_sec < 0 {
                bail!(
                    InvalidArgument,
                    msg(
                        "flush_if_sec for {} must be non-negative",
                        stream_type.as_str()
                    )
                );
            }
            change.config.flush_if_sec = flush_if_sec as u32;
        }

        if let Some(rtsp_transport) = subset.rtsp_transport.take() {
            // Validate transport option
            retina::client::Transport::from_str(rtsp_transport).map_err(|_| {
                err!(
                    InvalidArgument,
                    msg("invalid RTSP transport: {}", rtsp_transport)
                )
            })?;
            change.config.rtsp_transport = rtsp_transport.to_owned();
        }

        if let Some(sample_file_dir_id) = subset.sample_file_dir_id.take() {
            change.sample_file_dir_id = sample_file_dir_id;
        }

        // Safety valve in case something is added to StreamSubset and forgotten here.
        if subset != Default::default() {
            bail!(
                Unimplemented,
                msg("stream updates not supported: {subset:#?}"),
            );
        }

        Ok(())
    }

    /// Check camera preconditions for PATCH operations.
    fn check_camera_precondition(
        &self,
        db: &db::LockedDatabase,
        camera_id: i32,
        mut precondition: json::CameraSubset,
    ) -> Result<(), Error> {
        let camera = db.cameras_by_id().get(&camera_id).unwrap();

        if let Some(short_name) = precondition.short_name.take() {
            if short_name != camera.short_name {
                bail!(FailedPrecondition, msg("short_name mismatch"));
            }
        }

        if let Some(description) = precondition.description.take() {
            if description != camera.config.description {
                bail!(FailedPrecondition, msg("description mismatch"));
            }
        }

        if let Some(onvif_base_url) = precondition.onvif_base_url.take() {
            let expected_url = camera
                .config
                .onvif_base_url
                .as_ref()
                .map(|u| u.as_str())
                .unwrap_or("");
            if onvif_base_url != expected_url {
                bail!(FailedPrecondition, msg("onvif_base_url mismatch"));
            }
        }

        if let Some(username) = precondition.username.take() {
            if username != camera.config.username {
                bail!(FailedPrecondition, msg("username mismatch"));
            }
        }

        // We don't check password preconditions for security reasons

        // TODO: Add stream precondition checks if needed

        // Safety valve
        if precondition != Default::default() {
            bail!(
                Unimplemented,
                msg("preconditions not supported: {precondition:#?}"),
            );
        }

        Ok(())
    }

    /// Parse a URL with validation for allowed schemes.
    fn parse_url(
        &self,
        field_name: &str,
        raw: &str,
        allowed_schemes: &'static [&'static str],
    ) -> Result<url::Url, Error> {
        let url = url::Url::parse(raw).map_err(|_| {
            err!(
                InvalidArgument,
                msg("can't parse {} {:?} as URL", field_name, raw)
            )
        })?;

        if !allowed_schemes.iter().any(|scheme| *scheme == url.scheme()) {
            bail!(
                InvalidArgument,
                msg(
                    "unexpected scheme in {} {:?}; should be one of: {}",
                    field_name,
                    url.as_str(),
                    allowed_schemes.join(", "),
                ),
            );
        }

        if !url.username().is_empty() || url.password().is_some() {
            bail!(
                InvalidArgument,
                msg(
                    "unexpected credentials in {} {:?}; use the username and password fields instead",
                    field_name,
                    url.as_str(),
                ),
            );
        }

        Ok(url)
    }

    /// Parse a stream URL with RTSP validation.
    fn parse_stream_url(&self, stream_type: db::StreamType, raw: &str) -> Result<url::Url, Error> {
        self.parse_url(
            &format!("{} stream url", stream_type.as_str()),
            raw,
            &["rtsp"],
        )
    }

    /// Test camera stream connection asynchronously.
    async fn test_camera_stream(
        &self,
        url: url::Url,
        camera_config: &db::json::CameraConfig,
    ) -> Result<String, Error> {
        let credentials = if camera_config.username.is_empty() {
            None
        } else {
            Some(retina::client::Credentials {
                username: camera_config.username.clone(),
                password: camera_config.password.clone(),
            })
        };

        let options = stream::Options {
            session: retina::client::SessionOptions::default().creds(credentials),
            setup: retina::client::SetupOptions::default(),
        };

        let stream = stream::OPENER.open("test stream".to_owned(), url, options)?;
        let video_sample_entry = stream.video_sample_entry();

        Ok(format!(
            "codec: {}\n\
             dimensions: {}x{}\n\
             pixel aspect ratio: {}x{}\n\
             tool: {:?}",
            &video_sample_entry.rfc6381_codec,
            video_sample_entry.width,
            video_sample_entry.height,
            video_sample_entry.pasp_h_spacing,
            video_sample_entry.pasp_v_spacing,
            stream.tool(),
        ))
    }
}
