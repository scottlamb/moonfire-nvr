// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2026 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Sample file directory configuration API: `/api/config/sample-file-dirs`.

use base::{bail, err};
use http::{Method, Request, StatusCode};

use crate::json;

use super::{
    into_json_body, parse_json_body, plain_response, require_csrf_if_session, serve_json, Caller,
    ResponseResult, Service,
};

fn require_read(caller: &Caller) -> Result<(), base::Error> {
    if !caller.permissions.read_camera_configs && !caller.permissions.admin_camera_configs {
        bail!(
            PermissionDenied,
            msg("read_camera_configs or admin_camera_configs permission required")
        );
    }
    Ok(())
}

fn require_admin(caller: &Caller) -> Result<(), base::Error> {
    if !caller.permissions.admin_camera_configs {
        bail!(
            PermissionDenied,
            msg("admin_camera_configs permission required")
        );
    }
    Ok(())
}

impl Service {
    pub(super) async fn config_sample_file_dirs(
        &self,
        req: Request<hyper::body::Incoming>,
        caller: Caller,
    ) -> ResponseResult {
        match *req.method() {
            Method::GET | Method::HEAD => {
                require_read(&caller)?;
                let db = self.db.lock();
                let sample_file_dirs = db
                    .sample_file_dirs_by_id()
                    .values()
                    .map(|dir| json::SampleFileDirView {
                        id: dir.id,
                        path: dir.pool().path().to_owned(),
                    })
                    .collect();
                serve_json(&req, &json::SampleFileDirListResponse { sample_file_dirs })
            }
            Method::POST => {
                require_admin(&caller)?;
                let (parts, body) = into_json_body(req).await?;
                let request: json::SampleFileDirMutation = parse_json_body(&body)?;
                require_csrf_if_session(&caller, request.csrf)?;

                if !request.path.is_absolute() {
                    bail!(
                        InvalidArgument,
                        msg("sample file directory path must be absolute")
                    );
                }

                {
                    let db = self.db.lock();
                    if db
                        .sample_file_dirs_by_id()
                        .values()
                        .any(|dir| dir.pool().path() == request.path.as_path())
                    {
                        bail!(
                            FailedPrecondition,
                            msg(
                                "sample file directory {} is already configured",
                                request.path.display()
                            )
                        );
                    }
                }

                let id = self.db.add_sample_file_dir(request.path).await?;
                let path = {
                    let db = self.db.lock();
                    let dir = db.sample_file_dirs_by_id().get(&id).ok_or_else(|| {
                        err!(
                            Internal,
                            msg("sample file directory {id} missing after insert")
                        )
                    })?;
                    dir.pool().path().to_owned()
                };

                let mut response = serve_json(
                    &parts,
                    &json::SampleFileDirMutationResponse {
                        sample_file_dir: json::SampleFileDirView { id, path },
                        restart_required: false,
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
    async fn sample_file_dir_list_and_add() {
        testutil::init();
        let server = crate::web::tests::Server::new(Some(admin_permissions())).await;
        let client = reqwest::Client::new();
        let url = format!("{}/api/config/sample-file-dirs", server.base_url);

        let response = client.get(&url).send().await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let before: serde_json::Value = response.json().await.unwrap();
        assert_eq!(before["sampleFileDirs"].as_array().unwrap().len(), 1);

        let new_dir = server.db.tmpdir.path().join("api-recordings");
        std::fs::create_dir(&new_dir).unwrap();

        let response = client
            .post(&url)
            .json(&json!({"path": new_dir}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::CREATED);
        let created: serde_json::Value = response.json().await.unwrap();
        assert_eq!(created["restartRequired"], false);
        assert_eq!(
            created["sampleFileDir"]["path"].as_str().unwrap(),
            new_dir.to_str().unwrap()
        );

        let response = client.get(&url).send().await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let after: serde_json::Value = response.json().await.unwrap();
        assert_eq!(after["sampleFileDirs"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn sample_file_dir_requires_absolute_path() {
        testutil::init();
        let server = crate::web::tests::Server::new(Some(admin_permissions())).await;
        let client = reqwest::Client::new();
        let url = format!("{}/api/config/sample-file-dirs", server.base_url);

        let response = client
            .post(&url)
            .json(&json!({"path": "relative/path"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    }
}
