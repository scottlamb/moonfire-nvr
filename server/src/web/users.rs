// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2022 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! User management: `/api/users/*`.

use base::{bail_t, format_err_t};
use http::{Method, Request, StatusCode};

use crate::json::{self, PutUsersResponse, UserSubset};

use super::{
    bad_req, csrf_matches, extract_json_body, plain_response, serve_json, Caller, ResponseResult,
    Service,
};

impl Service {
    pub(super) async fn users(&self, req: Request<hyper::Body>, caller: Caller) -> ResponseResult {
        match *req.method() {
            Method::GET | Method::HEAD => self.get_users(req, caller).await,
            Method::PUT => self.put_users(req, caller).await,
            _ => Err(
                plain_response(StatusCode::METHOD_NOT_ALLOWED, "GET, HEAD, or PUT expected").into(),
            ),
        }
    }

    async fn get_users(&self, req: Request<hyper::Body>, caller: Caller) -> ResponseResult {
        if !caller.permissions.admin_users {
            bail_t!(Unauthenticated, "must have admin_users permission");
        }
        let users = self
            .db
            .lock()
            .users_by_id()
            .iter()
            .map(|(&id, user)| (id, user.username.clone()))
            .collect();
        serve_json(&req, &json::GetUsersResponse { users })
    }

    async fn put_users(&self, mut req: Request<hyper::Body>, caller: Caller) -> ResponseResult {
        if !caller.permissions.admin_users {
            bail_t!(Unauthenticated, "must have admin_users permission");
        }
        let r = extract_json_body(&mut req).await?;
        let r: json::UserSubset = serde_json::from_slice(&r).map_err(|e| bad_req(e.to_string()))?;
        let username = r
            .username
            .ok_or_else(|| format_err_t!(InvalidArgument, "username must be specified"))?;
        let mut change = db::UserChange::add_user(username.to_owned());
        if let Some(Some(pwd)) = r.password {
            change.set_password(pwd.to_owned());
        }
        if let Some(preferences) = r.preferences {
            change.config.preferences = preferences;
        }
        if let Some(ref permissions) = r.permissions {
            change.permissions = permissions.into();
        }
        let mut l = self.db.lock();
        let user = l.apply_user_change(change)?;
        serve_json(&req, &PutUsersResponse { id: user.id })
    }

    pub(super) async fn user(
        &self,
        req: Request<hyper::Body>,
        caller: Caller,
        id: i32,
    ) -> ResponseResult {
        match *req.method() {
            Method::GET | Method::HEAD => self.get_user(req, caller, id).await,
            Method::DELETE => self.delete_user(caller, id).await,
            Method::POST => self.post_user(req, caller, id).await,
            _ => Err(plain_response(
                StatusCode::METHOD_NOT_ALLOWED,
                "GET, HEAD, DELETE, or POST expected",
            )
            .into()),
        }
    }

    async fn get_user(&self, req: Request<hyper::Body>, caller: Caller, id: i32) -> ResponseResult {
        require_same_or_admin(&caller, id)?;
        let db = self.db.lock();
        let user = db
            .users_by_id()
            .get(&id)
            .ok_or_else(|| format_err_t!(NotFound, "can't find requested user"))?;
        let out = UserSubset {
            username: Some(&user.username),
            preferences: Some(user.config.preferences.clone()),
            password: Some(if user.has_password() {
                Some("(censored)")
            } else {
                None
            }),
            permissions: Some((&user.permissions).into()),
        };
        serve_json(&req, &out)
    }

    async fn delete_user(&self, caller: Caller, id: i32) -> ResponseResult {
        if !caller.permissions.admin_users {
            bail_t!(Unauthenticated, "must have admin_users permission");
        }
        let mut l = self.db.lock();
        l.delete_user(id)?;
        Ok(plain_response(StatusCode::NO_CONTENT, &b""[..]))
    }

    async fn post_user(
        &self,
        mut req: Request<hyper::Body>,
        caller: Caller,
        id: i32,
    ) -> ResponseResult {
        require_same_or_admin(&caller, id)?;
        let r = extract_json_body(&mut req).await?;
        let r: json::PostUser = serde_json::from_slice(&r).map_err(|e| bad_req(e.to_string()))?;
        let mut db = self.db.lock();
        let user = db
            .get_user_by_id_mut(id)
            .ok_or_else(|| format_err_t!(NotFound, "can't find requested user"))?;
        if r.update.as_ref().map(|u| u.password).is_some()
            && r.precondition.as_ref().map(|p| p.password).is_none()
            && !caller.permissions.admin_users
        {
            bail_t!(
                Unauthenticated,
                "to change password, must supply previous password or have admin_users permission"
            );
        }
        if r.update.as_ref().map(|u| &u.permissions).is_some() && !caller.permissions.admin_users {
            bail_t!(
                Unauthenticated,
                "to change permissions, must have admin_users permission"
            );
        }
        match (r.csrf, caller.user.and_then(|u| u.session)) {
            (None, Some(_)) => bail_t!(Unauthenticated, "csrf must be supplied"),
            (Some(csrf), Some(session)) if !csrf_matches(csrf, session.csrf) => {
                bail_t!(Unauthenticated, "incorrect csrf");
            }
            (_, _) => {}
        }
        if let Some(ref precondition) = r.precondition {
            if matches!(precondition.preferences, Some(ref p) if p != &user.config.preferences) {
                bail_t!(FailedPrecondition, "preferences mismatch");
            }
            if let Some(p) = precondition.password {
                if !user.check_password(p)? {
                    bail_t!(FailedPrecondition, "password mismatch"); // or Unauthenticated?
                }
            }
            if let Some(ref p) = precondition.permissions {
                if user.permissions != db::Permissions::from(p) {
                    bail_t!(FailedPrecondition, "permissions mismatch");
                }
            }
        }
        if let Some(update) = r.update {
            let mut change = user.change();
            if let Some(preferences) = update.preferences {
                change.config.preferences = preferences;
            }
            match update.password {
                None => {}
                Some(None) => change.clear_password(),
                Some(Some(p)) => change.set_password(p.to_owned()),
            }
            if let Some(ref permissions) = update.permissions {
                change.permissions = permissions.into();
            }
            db.apply_user_change(change)?;
        }
        Ok(plain_response(StatusCode::NO_CONTENT, &b""[..]))
    }
}

fn require_same_or_admin(caller: &Caller, id: i32) -> Result<(), base::Error> {
    if caller.user.as_ref().map(|u| u.id) != Some(id) && !caller.permissions.admin_users {
        bail_t!(
            Unauthenticated,
            "must be authenticated as supplied user or have admin_users permission"
        );
    }
    Ok(())
}
