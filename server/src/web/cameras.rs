// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2026 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Camera and stream configuration API: `/api/config/cameras`.

use base::{bail, err, Error};
use http::{Method, Request, StatusCode};
use std::collections::BTreeMap;
use std::str::FromStr;
use uuid::Uuid;

use crate::json;

use super::{
    into_json_body, parse_json_body, plain_response, require_csrf_if_session, serve_json, Caller,
    ResponseResult, Service,
};

fn require_read(caller: &Caller) -> Result<(), Error> {
    if !caller.permissions.read_camera_configs && !caller.permissions.admin_camera_configs {
        bail!(
            PermissionDenied,
            msg("read_camera_configs or admin_camera_configs permission required")
        );
    }
    Ok(())
}

fn require_admin(caller: &Caller) -> Result<(), Error> {
    if !caller.permissions.admin_camera_configs {
        bail!(
            PermissionDenied,
            msg("admin_camera_configs permission required")
        );
    }
    Ok(())
}

fn camera_view(
    db: &db::LockedDatabase,
    camera: &db::Camera,
) -> Result<json::CameraConfigView, Error> {
    let mut streams = BTreeMap::new();
    for &type_ in &db::ALL_STREAM_TYPES {
        let Some(stream_id) = camera.streams[type_.index()] else {
            continue;
        };
        let stream = db.streams_by_id().get(&stream_id).ok_or_else(|| {
            err!(
                Internal,
                msg("camera references missing stream {stream_id}")
            )
        })?;
        let stream = stream.inner.lock();
        streams.insert(
            type_.as_str().to_owned(),
            json::StreamConfigView {
                id: stream_id,
                sample_file_dir_id: stream.sample_file_dir.as_ref().map(|d| d.id),
                config: stream.config.clone(),
            },
        );
    }
    Ok(json::CameraConfigView {
        uuid: camera.uuid,
        id: camera.id,
        short_name: camera.short_name.clone(),
        config: camera.config.clone(),
        streams,
    })
}

fn validate_url(field: &str, url: &url::Url, allowed_schemes: &[&str]) -> Result<(), Error> {
    if !allowed_schemes.iter().any(|scheme| *scheme == url.scheme()) {
        bail!(
            InvalidArgument,
            msg(
                "unexpected scheme in {field} {:?}; should be one of: {}",
                url.as_str(),
                allowed_schemes.join(", ")
            )
        );
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!(
            InvalidArgument,
            msg(
                "unexpected credentials in {field} {:?}; use camera username/password instead",
                url.as_str()
            )
        );
    }
    Ok(())
}

fn camera_change(
    db: &db::LockedDatabase,
    input: json::CameraConfigInput,
) -> Result<db::CameraChange, Error> {
    if let Some(url) = input.config.onvif_base_url.as_ref() {
        validate_url("onvifBaseUrl", url, &["http", "https"])?;
    }

    let mut change = db::CameraChange {
        short_name: input.short_name,
        config: input.config,
        streams: Default::default(),
    };

    for (name, stream) in input.streams {
        let type_ = db::StreamType::parse(&name)
            .ok_or_else(|| err!(InvalidArgument, msg("unknown stream type {name:?}")))?;

        if let Some(dir_id) = stream.sample_file_dir_id {
            if !db.sample_file_dirs_by_id().contains_key(&dir_id) {
                bail!(
                    InvalidArgument,
                    msg("no such sample file directory {dir_id}")
                );
            }
        }

        if let Some(url) = stream.config.url.as_ref() {
            validate_url(&format!("{name} stream URL"), url, &["rtsp"])?;
        }

        if !stream.config.rtsp_transport.is_empty()
            && retina::client::Transport::from_str(&stream.config.rtsp_transport).is_err()
        {
            bail!(
                InvalidArgument,
                msg(
                    "invalid RTSP transport {:?} for {name} stream",
                    stream.config.rtsp_transport
                )
            );
        }

        if stream.config.retain_bytes < 0 {
            bail!(
                InvalidArgument,
                msg("retainBytes for {name} stream must be non-negative")
            );
        }

        if stream.config.mode == db::json::STREAM_MODE_RECORD
            && (stream.config.url.is_none() || stream.sample_file_dir_id.is_none())
        {
            bail!(
                InvalidArgument,
                msg("can't record {name} stream without RTSP URL and sample file directory")
            );
        }

        change.streams[type_.index()] = db::StreamChange {
            sample_file_dir_id: stream.sample_file_dir_id,
            config: stream.config,
        };
    }

    Ok(change)
}

fn ensure_safe_update(
    db: &db::LockedDatabase,
    camera: &db::Camera,
    change: &db::CameraChange,
) -> Result<(), Error> {
    for i in 0..db::NUM_STREAM_TYPES {
        let Some(stream_id) = camera.streams[i] else {
            continue;
        };
        let stream = db.streams_by_id().get(&stream_id).ok_or_else(|| {
            err!(
                Internal,
                msg("camera references missing stream {stream_id}")
            )
        })?;
        let stream = stream.inner.lock();
        if !stream.has_open_writer() {
            continue;
        }
        let old_dir = stream.sample_file_dir.as_ref().map(|d| d.id);
        let new_dir = change.streams[i].sample_file_dir_id;
        if old_dir != new_dir {
            bail!(
                FailedPrecondition,
                msg(
                    "can't change sample file directory for actively writing stream {stream_id}; restart/stop recording first"
                )
            );
        }
    }
    Ok(())
}

fn ensure_safe_delete(db: &db::LockedDatabase, camera: &db::Camera) -> Result<(), Error> {
    for stream_id in camera.streams.iter().flatten().copied() {
        let stream = db.streams_by_id().get(&stream_id).ok_or_else(|| {
            err!(
                Internal,
                msg("camera references missing stream {stream_id}")
            )
        })?;
        if stream.inner.lock().has_open_writer() {
            bail!(
                FailedPrecondition,
                msg("can't delete camera while stream {stream_id} is actively writing")
            );
        }
    }
    Ok(())
}

impl Service {
    pub(super) async fn config_cameras(
        &self,
        req: Request<hyper::body::Incoming>,
        caller: Caller,
    ) -> ResponseResult {
        match *req.method() {
            Method::GET | Method::HEAD => {
                require_read(&caller)?;
                let db = self.db.lock();
                let cameras = db
                    .cameras_by_id()
                    .values()
                    .map(|camera| camera_view(&db, camera))
                    .collect::<Result<Vec<_>, _>>()?;
                serve_json(&req, &json::CameraConfigListResponse { cameras })
            }
            Method::POST => {
                require_admin(&caller)?;
                let (parts, body) = into_json_body(req).await?;
                let request: json::CameraConfigMutation = parse_json_body(&body)?;
                require_csrf_if_session(&caller, request.csrf)?;

                let mut db = self.db.lock();
                let change = camera_change(&db, request.camera)?;
                let id = db.add_camera(change)?;
                let camera = db
                    .cameras_by_id()
                    .get(&id)
                    .ok_or_else(|| err!(Internal, msg("new camera {id} missing after insert")))?;
                let camera = camera_view(&db, camera)?;
                drop(db);

                let mut response = serve_json(
                    &parts,
                    &json::CameraConfigMutationResponse {
                        camera,
                        restart_required: true,
                    },
                )?;
                *response.status_mut() = StatusCode::CREATED;
                Ok(response)
            }
            _ => Ok(plain_response(
                StatusCode::METHOD_NOT_ALLOWED,
                "GET, HEAD, or POST expected",
            )),
        }
    }

    pub(super) async fn config_camera(
        &self,
        req: Request<hyper::body::Incoming>,
        caller: Caller,
        uuid: Uuid,
    ) -> ResponseResult {
        match *req.method() {
            Method::GET | Method::HEAD => {
                require_read(&caller)?;
                let db = self.db.lock();
                let camera = db
                    .get_camera(uuid)
                    .ok_or_else(|| err!(NotFound, msg("no such camera {uuid}")))?;
                serve_json(&req, &camera_view(&db, camera)?)
            }
            Method::PUT => {
                require_admin(&caller)?;
                let (parts, body) = into_json_body(req).await?;
                let request: json::CameraConfigMutation = parse_json_body(&body)?;
                require_csrf_if_session(&caller, request.csrf)?;

                let mut db = self.db.lock();
                let id = db
                    .get_camera(uuid)
                    .ok_or_else(|| err!(NotFound, msg("no such camera {uuid}")))?
                    .id;
                let change = camera_change(&db, request.camera)?;
                {
                    let camera = db
                        .cameras_by_id()
                        .get(&id)
                        .ok_or_else(|| err!(Internal, msg("camera {id} disappeared")))?;
                    ensure_safe_update(&db, camera, &change)?;
                }
                db.update_camera(id, change)?;
                let camera = db
                    .cameras_by_id()
                    .get(&id)
                    .ok_or_else(|| err!(Internal, msg("camera {id} missing after update")))?;
                let camera = camera_view(&db, camera)?;
                drop(db);

                serve_json(
                    &parts,
                    &json::CameraConfigMutationResponse {
                        camera,
                        restart_required: true,
                    },
                )
            }
            Method::DELETE => {
                require_admin(&caller)?;
                let (parts, body) = into_json_body(req).await?;
                let request: json::DeleteCameraConfig = parse_json_body(&body)?;
                require_csrf_if_session(&caller, request.csrf)?;

                let mut db = self.db.lock();
                let id = db
                    .get_camera(uuid)
                    .ok_or_else(|| err!(NotFound, msg("no such camera {uuid}")))?
                    .id;
                {
                    let camera = db
                        .cameras_by_id()
                        .get(&id)
                        .ok_or_else(|| err!(Internal, msg("camera {id} disappeared")))?;
                    ensure_safe_delete(&db, camera)?;
                }
                db.delete_camera(id)?;
                drop(db);

                serve_json(
                    &parts,
                    &json::DeleteCameraConfigResponse {
                        restart_required: true,
                    },
                )
            }
            _ => Ok(plain_response(
                StatusCode::METHOD_NOT_ALLOWED,
                "GET, HEAD, PUT, or DELETE expected",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use db::testutil;
    use serde_json::json;

    fn admin_permissions() -> db::Permissions {
        db::Permissions {
            read_camera_configs: true,
            admin_camera_configs: true,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn camera_config_crud() {
        testutil::init();
        let server = crate::web::tests::Server::new(Some(admin_permissions())).await;
        let client = reqwest::Client::new();
        let dir_id = *server
            .db
            .db
            .lock()
            .sample_file_dirs_by_id()
            .keys()
            .next()
            .unwrap();

        let collection = format!("{}/api/config/cameras", server.base_url);
        let create = json!({
            "camera": {
                "shortName": "api camera",
                "config": {
                    "description": "created through JSON API",
                    "username": "viewer",
                    "password": "secret"
                },
                "streams": {
                    "main": {
                        "sampleFileDirId": dir_id,
                        "config": {
                            "mode": "record",
                            "url": "rtsp://192.0.2.10/main",
                            "rtspTransport": "tcp",
                            "retainBytes": 10485760,
                            "flushIfSec": 5
                        }
                    }
                }
            }
        });
        let response = client.post(&collection).json(&create).send().await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::CREATED);
        let created: serde_json::Value = response.json().await.unwrap();
        assert_eq!(created["restartRequired"], true);
        let uuid = created["camera"]["uuid"].as_str().unwrap().to_owned();

        let item = format!("{collection}/{uuid}");
        let response = client.get(&item).send().await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let camera: serde_json::Value = response.json().await.unwrap();
        assert_eq!(camera["shortName"], "api camera");
        assert_eq!(camera["streams"]["main"]["sampleFileDirId"], dir_id);
        assert_eq!(camera["streams"]["main"]["config"]["mode"], "record");

        let update = json!({
            "camera": {
                "shortName": "api camera updated",
                "config": {
                    "description": "updated through JSON API",
                    "username": "viewer",
                    "password": "new-secret"
                },
                "streams": {
                    "main": {
                        "sampleFileDirId": dir_id,
                        "config": {
                            "mode": "",
                            "url": "rtsp://192.0.2.10/main",
                            "rtspTransport": "tcp",
                            "retainBytes": 10485760,
                            "flushIfSec": 5
                        }
                    }
                }
            }
        });
        let response = client.put(&item).json(&update).send().await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let updated: serde_json::Value = response.json().await.unwrap();
        assert_eq!(updated["restartRequired"], true);
        assert_eq!(updated["camera"]["shortName"], "api camera updated");
        assert!(updated["camera"]["streams"]["main"]["config"]["mode"].is_null());

        let response = client.delete(&item).json(&json!({})).send().await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let deleted: serde_json::Value = response.json().await.unwrap();
        assert_eq!(deleted["restartRequired"], true);

        let response = client.get(&item).send().await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn camera_config_permissions() {
        testutil::init();
        let server = crate::web::tests::Server::new(Some(db::Permissions {
            read_camera_configs: true,
            ..Default::default()
        }))
        .await;
        let client = reqwest::Client::new();
        let collection = format!("{}/api/config/cameras", server.base_url);

        let response = client.get(&collection).send().await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);

        let response = client
            .post(&collection)
            .json(&json!({
                "camera": {
                    "shortName": "forbidden",
                    "config": {},
                    "streams": {}
                }
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn camera_config_validation() {
        testutil::init();
        let server = crate::web::tests::Server::new(Some(admin_permissions())).await;
        let client = reqwest::Client::new();
        let collection = format!("{}/api/config/cameras", server.base_url);

        let response = client
            .post(&collection)
            .json(&json!({
                "camera": {
                    "shortName": "bad",
                    "config": {},
                    "streams": {
                        "main": {
                            "sampleFileDirId": 999999,
                            "config": {
                                "mode": "record",
                                "url": "rtsp://192.0.2.10/main"
                            }
                        }
                    }
                }
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    }
}
