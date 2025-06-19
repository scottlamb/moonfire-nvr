// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2024 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

//! Storage management API endpoints.

use crate::json;
use base::err;
use http::{Method, Request, StatusCode};
use std::path::PathBuf;

use super::{
    into_json_body, parse_json_body, plain_response, require_csrf_if_session, serve_json, Caller,
    ResponseResult, Service,
};

impl Service {
    pub(super) async fn storage(
        &self,
        req: Request<::hyper::body::Incoming>,
        caller: Caller,
    ) -> ResponseResult {
        let permissions = &caller.permissions;
        match *req.method() {
            Method::GET => {
                if !permissions.view_video {
                    return Err(err!(PermissionDenied, msg("view_video required")));
                }
                self.get_storage(&req, caller)
            }
            Method::POST => {
                if !permissions.admin_cameras {
                    return Err(err!(PermissionDenied, msg("admin_cameras required")));
                }
                self.post_storage(req, caller).await
            }
            _ => Ok(plain_response(
                StatusCode::METHOD_NOT_ALLOWED,
                "GET or POST expected",
            )),
        }
    }

    pub(super) async fn storage_dir(
        &self,
        req: Request<::hyper::body::Incoming>,
        caller: Caller,
        id: i32,
    ) -> ResponseResult {
        let permissions = &caller.permissions;
        match *req.method() {
            Method::GET => {
                if !permissions.view_video {
                    return Err(err!(PermissionDenied, msg("view_video required")));
                }
                self.get_storage_dir(&req, caller, id)
            }
            Method::PATCH => {
                if !permissions.admin_cameras {
                    return Err(err!(PermissionDenied, msg("admin_cameras required")));
                }
                self.patch_storage_dir(req, caller, id).await
            }
            Method::DELETE => {
                if !permissions.admin_cameras {
                    return Err(err!(PermissionDenied, msg("admin_cameras required")));
                }
                self.delete_storage_dir(req, caller, id).await
            }
            _ => Ok(plain_response(
                StatusCode::METHOD_NOT_ALLOWED,
                "GET, PATCH, or DELETE expected",
            )),
        }
    }

    fn get_storage(
        &self,
        req: &Request<::hyper::body::Incoming>,
        _caller: Caller,
    ) -> ResponseResult {
        let db = self.db.lock();
        let mut storage_dirs = Vec::new();

        for (&id, dir) in db.sample_file_dirs_by_id() {
            let mut total_bytes = 0i64;
            let mut used_bytes = 0i64;
            let mut streams_using = Vec::new();

            // Calculate usage by streams
            for (&stream_id, stream) in db.streams_by_id() {
                if stream.sample_file_dir_id == Some(id) {
                    used_bytes += stream.sample_file_bytes;
                    streams_using.push(json::StorageStreamUsage {
                        stream_id,
                        camera_name: db
                            .cameras_by_id()
                            .get(&stream.camera_id)
                            .map(|c| c.short_name.clone())
                            .unwrap_or_else(|| format!("camera {}", stream.camera_id)),
                        stream_type: stream.type_.as_str().to_string(),
                        used_bytes: stream.sample_file_bytes,
                        duration_90k: stream.duration.0,
                    });
                }
            }

            // Try to get filesystem stats if directory is accessible
            if let Ok(dir_handle) = dir.get() {
                if let Ok(stat) = dir_handle.statfs() {
                    total_bytes = (stat.blocks_available() * stat.fragment_size()) as i64;
                }
            }

            storage_dirs.push(json::StorageDir {
                id,
                uuid: dir.uuid,
                path: dir.path.display().to_string(),
                total_bytes,
                used_bytes,
                streams_using,
            });
        }

        serve_json(req, &json::GetStorageResponse { storage_dirs })
    }

    fn get_storage_dir(
        &self,
        req: &Request<::hyper::body::Incoming>,
        _caller: Caller,
        id: i32,
    ) -> ResponseResult {
        let db = self.db.lock();
        let dir = db
            .sample_file_dirs_by_id()
            .get(&id)
            .ok_or_else(|| err!(NotFound, msg("no such storage directory {id}")))?;

        let mut used_bytes = 0i64;
        let mut streams_using = Vec::new();

        // Calculate usage by streams
        for (&stream_id, stream) in db.streams_by_id() {
            if stream.sample_file_dir_id == Some(id) {
                used_bytes += stream.sample_file_bytes;
                streams_using.push(json::StorageStreamUsage {
                    stream_id,
                    camera_name: db
                        .cameras_by_id()
                        .get(&stream.camera_id)
                        .map(|c| c.short_name.clone())
                        .unwrap_or_else(|| format!("camera {}", stream.camera_id)),
                    stream_type: stream.type_.as_str().to_string(),
                    used_bytes: stream.sample_file_bytes,
                    duration_90k: stream.duration.0,
                });
            }
        }

        let mut total_bytes = 0i64;
        if let Ok(dir_handle) = dir.get() {
            if let Ok(stat) = dir_handle.statfs() {
                total_bytes = (stat.blocks_available() * stat.fragment_size()) as i64;
            }
        }

        let storage_dir = json::StorageDir {
            id,
            uuid: dir.uuid,
            path: dir.path.display().to_string(),
            total_bytes,
            used_bytes,
            streams_using,
        };

        serve_json(req, &storage_dir)
    }

    async fn post_storage(
        &self,
        req: Request<::hyper::body::Incoming>,
        caller: Caller,
    ) -> ResponseResult {
        let (parts, b) = into_json_body(req).await?;
        let r: json::PostStorageRequest = parse_json_body(&b)?;
        require_csrf_if_session(&caller, r.csrf)?;

        let mut db = self.db.lock();
        let id = db.add_sample_file_dir(PathBuf::from(r.path))?;

        // Get the UUID from the created directory
        let uuid = db
            .sample_file_dirs_by_id()
            .get(&id)
            .ok_or_else(|| err!(Internal, msg("directory not found after creation")))?
            .uuid;

        let (parts, _) = (parts, ());
        serve_json(&parts, &json::PostStorageResponse { id, uuid })
    }

    async fn patch_storage_dir(
        &self,
        req: Request<::hyper::body::Incoming>,
        caller: Caller,
        _id: i32,
    ) -> ResponseResult {
        let (parts, b) = into_json_body(req).await?;
        let r: json::PatchStorageRequest = parse_json_body(&b)?;
        require_csrf_if_session(&caller, r.csrf)?;

        // For now, storage directories can't be updated - they're essentially immutable
        // once created. This endpoint exists for future extensibility.
        let _ = r; // Silence unused variable warning

        let (parts, _) = (parts, ());
        serve_json(&parts, &json::PatchStorageResponse { success: true })
    }

    async fn delete_storage_dir(
        &self,
        req: Request<::hyper::body::Incoming>,
        caller: Caller,
        id: i32,
    ) -> ResponseResult {
        let (parts, b) = into_json_body(req).await?;
        let r: json::DeleteStorageRequest = parse_json_body(&b)?;
        require_csrf_if_session(&caller, r.csrf)?;

        let mut db = self.db.lock();
        db.delete_sample_file_dir(id)?;

        let (parts, _) = (parts, ());
        serve_json(&parts, &json::DeleteStorageResponse { success: true })
    }

    pub(super) fn storage_dirs_simple(
        &self,
        req: Request<::hyper::body::Incoming>,
        caller: Caller,
    ) -> ResponseResult {
        let permissions = &caller.permissions;
        if !permissions.view_video {
            return Err(err!(PermissionDenied, msg("view_video required")));
        }

        let db = self.db.lock();
        let mut dirs = Vec::new();

        for (&id, dir) in db.sample_file_dirs_by_id() {
            dirs.push(json::StorageDirSimple {
                id,
                path: dir.path.display().to_string(),
            });
        }

        let (parts, _) = req.into_parts();
        serve_json(&parts, &json::GetStorageDirsSimpleResponse { dirs })
    }
}
