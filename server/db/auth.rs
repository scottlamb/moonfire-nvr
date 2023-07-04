// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2018 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Authentication schema: users and sessions/cookies.

use crate::json::UserConfig;
use crate::schema::Permissions;
use base::{bail_t, format_err_t, strutil, ErrorKind, ResultExt as _};
use failure::{bail, format_err, Error, Fail, ResultExt as _};
use fnv::FnvHashMap;
use protobuf::Message;
use ring::rand::{SecureRandom, SystemRandom};
use rusqlite::{named_params, params, Connection, Transaction};
use scrypt::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use std::collections::BTreeMap;
use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::OnceLock;
use tracing::info;

/// Wrapper around [`scrypt::Params`].
///
/// `scrypt::Params` does not implement `PartialEq`; so for the benefit of `set_test_config`
/// error handling, keep track of whether these params are the recommended
/// production ones or the cheap test ones.
struct Params {
    actual: scrypt::Params,
    is_test: bool,
}

static PARAMS: OnceLock<Params> = OnceLock::new();

fn params() -> &'static Params {
    PARAMS.get_or_init(|| Params {
        actual: scrypt::Params::recommended(),
        is_test: false,
    })
}

/// For testing only: use fast but insecure hashes.
/// Call via `testutil::init()`.
pub(crate) fn set_test_config() {
    let test_params = scrypt::Params::new(8, 8, 1).expect("test params should be valid");
    if let Err(existing_params) = PARAMS.set(Params {
        actual: test_params,
        is_test: true,
    }) {
        assert!(
            existing_params.is_test,
            "set_test_config must be called before any use of the parameters"
        );
    }
}

#[derive(Debug)]
pub struct User {
    pub id: i32,
    pub username: String,
    pub config: UserConfig,
    password_hash: Option<String>,
    pub password_id: i32,
    pub password_failure_count: i64,
    pub permissions: Permissions,

    /// True iff this `User` has changed since the last flush.
    /// Only a couple things are flushed lazily: `password_failure_count` and (on upgrade to a new
    /// algorithm) `password_hash`.
    dirty: bool,
}

impl User {
    pub fn change(&self) -> UserChange {
        UserChange {
            id: Some(self.id),
            username: self.username.clone(),
            config: self.config.clone(),
            set_password_hash: None,
            permissions: self.permissions.clone(),
        }
    }

    pub fn has_password(&self) -> bool {
        self.password_hash.is_some()
    }

    /// Checks if the user's password hash matches the supplied password.
    ///
    /// As a side effect, increments `password_failure_count` and sets `dirty`
    /// if `password` is incorrect.
    pub fn check_password(&mut self, password: Option<&str>) -> Result<bool, base::Error> {
        let hash = self.password_hash.as_ref();
        let (password, hash) = match (password, hash) {
            (None, None) => return Ok(true),
            (Some(p), Some(h)) => (p, h),
            _ => return Ok(false),
        };
        let hash = PasswordHash::new(hash)
            .with_context(|_| {
                format!(
                    "bad stored password hash for user {:?}: {:?}",
                    self.username, hash
                )
            })
            .context(ErrorKind::DataLoss)?;
        match scrypt::Scrypt.verify_password(password.as_bytes(), &hash) {
            Ok(()) => Ok(true),
            Err(scrypt::password_hash::errors::Error::Password) => {
                self.dirty = true;
                self.password_failure_count += 1;
                Ok(false)
            }
            Err(e) => Err(e
                .context(format!(
                    "unable to verify password for user {:?}",
                    self.username
                ))
                .context(ErrorKind::Internal)
                .into()),
        }
    }
}

/// A change to a user.
///
///    * an insertion returned via `UserChange::add_user`.
///    * an update returned via `User::change`.
///
/// Apply via `DatabaseGuard::apply_user_change` (which internally calls `auth::State::apply`).
#[derive(Clone, Debug)]
pub struct UserChange {
    id: Option<i32>,
    pub username: String,
    pub config: UserConfig,
    set_password_hash: Option<Option<String>>,
    pub permissions: Permissions,
}

impl UserChange {
    pub fn add_user(username: String) -> Self {
        UserChange {
            id: None,
            username,
            config: UserConfig::default(),
            set_password_hash: None,
            permissions: Permissions::default(),
        }
    }

    pub fn set_password(&mut self, pwd: String) {
        let salt = SaltString::generate(&mut scrypt::password_hash::rand_core::OsRng);
        let params = params();
        let hash = scrypt::Scrypt
            .hash_password_customized(pwd.as_bytes(), None, None, params.actual, &salt)
            .unwrap();
        self.set_password_hash = Some(Some(hash.to_string()));
    }

    pub fn clear_password(&mut self) {
        self.set_password_hash = Some(None);
    }
}

#[derive(Clone, Debug, Default)]
pub struct Request {
    pub when_sec: Option<i64>,
    pub user_agent: Option<Vec<u8>>,
    pub addr: Option<IpAddr>,
}

impl Request {
    fn addr_buf(&self) -> Option<IpAddrBuf> {
        match self.addr {
            None => None,
            Some(IpAddr::V4(ref a)) => Some(IpAddrBuf::V4(a.octets())),
            Some(IpAddr::V6(ref a)) => Some(IpAddrBuf::V6(a.octets())),
        }
    }
}

enum IpAddrBuf {
    V4([u8; 4]),
    V6([u8; 16]),
}

impl AsRef<[u8]> for IpAddrBuf {
    fn as_ref(&self) -> &[u8] {
        match *self {
            IpAddrBuf::V4(ref s) => &s[..],
            IpAddrBuf::V6(ref s) => &s[..],
        }
    }
}

pub struct FromSqlIpAddr(Option<IpAddr>);

impl rusqlite::types::FromSql for FromSqlIpAddr {
    fn column_result(value: rusqlite::types::ValueRef) -> rusqlite::types::FromSqlResult<Self> {
        use rusqlite::types::ValueRef;
        match value {
            ValueRef::Null => Ok(FromSqlIpAddr(None)),
            ValueRef::Blob(b) => match b.len() {
                4 => {
                    let mut buf = [0u8; 4];
                    buf.copy_from_slice(b);
                    Ok(FromSqlIpAddr(Some(buf.into())))
                }
                16 => {
                    let mut buf = [0u8; 16];
                    buf.copy_from_slice(b);
                    Ok(FromSqlIpAddr(Some(buf.into())))
                }
                _ => Err(rusqlite::types::FromSqlError::InvalidType),
            },
            _ => Err(rusqlite::types::FromSqlError::InvalidType),
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum SessionFlag {
    HttpOnly = 1,
    Secure = 2,
    SameSite = 4,
    SameSiteStrict = 8,
}

impl FromStr for SessionFlag {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "http-only" => Ok(Self::HttpOnly),
            "secure" => Ok(Self::Secure),
            "same-site" => Ok(Self::SameSite),
            "same-site-strict" => Ok(Self::SameSiteStrict),
            _ => bail!("No such session flag {:?}", s),
        }
    }
}

#[derive(Copy, Clone)]
pub enum RevocationReason {
    LoggedOut = 1,
    AlgorithmChange = 2,
}

#[allow(dead_code)] // Some of these fields are currently only used in Debug. That's fine.
#[derive(Debug, Default)]
pub struct Session {
    pub user_id: i32,
    flags: i32, // bitmask of SessionFlag enum values
    domain: Option<Vec<u8>>,
    description: Option<String>,
    seed: Seed,

    creation_password_id: Option<i32>,
    creation: Request,

    revocation: Request,
    revocation_reason: Option<i32>, // see RevocationReason enum
    revocation_reason_detail: Option<String>,

    pub permissions: Permissions,

    last_use: Request,
    use_count: i32,
    dirty: bool,
}

impl Session {
    pub fn csrf(&self) -> SessionHash {
        let r = blake3::keyed_hash(&self.seed.0, b"csrf");
        let mut h = SessionHash([0u8; 24]);
        h.0.copy_from_slice(&r.as_bytes()[0..24]);
        h
    }
}

/// A raw session id (not base64-encoded). Sensitive. Never stored in the database.
#[derive(Copy, Clone)]
pub struct RawSessionId([u8; 48]);

impl RawSessionId {
    pub fn decode_base64(input: &[u8]) -> Result<Self, Error> {
        let mut s = RawSessionId([0u8; 48]);
        let l = ::base64::decode_config_slice(input, ::base64::STANDARD_NO_PAD, &mut s.0[..])?;
        if l != 48 {
            bail!("session id must be 48 bytes");
        }
        Ok(s)
    }

    pub fn hash(&self) -> SessionHash {
        let r = blake3::hash(&self.0[..]);
        let mut h = SessionHash([0u8; 24]);
        h.0.copy_from_slice(&r.as_bytes()[0..24]);
        h
    }
}

impl AsRef<[u8]> for RawSessionId {
    fn as_ref(&self) -> &[u8] {
        &self.0[..]
    }
}

impl AsMut<[u8]> for RawSessionId {
    fn as_mut(&mut self) -> &mut [u8] {
        &mut self.0[..]
    }
}

impl fmt::Debug for RawSessionId {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(f, "RawSessionId(\"{}\")", &strutil::hex(&self.0[..]))
    }
}

/// A Blake2b-256 (48 bytes) of data associated with the session.
/// This is currently used in two ways:
/// *   the csrf token is a truncated blake3 derived from the session's seed. This is put into the
///     `sc` cookie.
/// *   the 48-byte session id is hashed to be used as a database key.
#[derive(Copy, Clone, Default, PartialEq, Eq, Hash)]
pub struct SessionHash(pub [u8; 24]);

impl SessionHash {
    pub fn encode_base64(&self, output: &mut [u8; 32]) {
        ::base64::encode_config_slice(self.0, ::base64::STANDARD_NO_PAD, output);
    }

    pub fn decode_base64(input: &[u8]) -> Result<Self, Error> {
        let mut h = SessionHash([0u8; 24]);
        let l = ::base64::decode_config_slice(input, ::base64::STANDARD_NO_PAD, &mut h.0[..])?;
        if l != 24 {
            bail!("session hash must be 24 bytes");
        }
        Ok(h)
    }
}

impl fmt::Debug for SessionHash {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        let mut buf = [0; 32];
        self.encode_base64(&mut buf);
        write!(
            f,
            "SessionHash(\"{}\")",
            ::std::str::from_utf8(&buf[..]).expect("base64 is UTF-8")
        )
    }
}

#[derive(Copy, Clone, Debug, Default)]
struct Seed([u8; 32]);

impl rusqlite::types::FromSql for Seed {
    fn column_result(value: rusqlite::types::ValueRef) -> rusqlite::types::FromSqlResult<Self> {
        let b = value.as_blob()?;
        if b.len() != 32 {
            return Err(rusqlite::types::FromSqlError::Other(Box::new(
                format_err!("expected a 32-byte seed").compat(),
            )));
        }
        let mut s = Seed::default();
        s.0.copy_from_slice(b);
        Ok(s)
    }
}

pub(crate) struct State {
    users_by_id: BTreeMap<i32, User>,
    users_by_name: BTreeMap<String, i32>,

    /// Some of the sessions stored in the database.
    /// Guaranteed to contain all "dirty" sessions (ones with unflushed changes); may contain
    /// others.
    ///
    /// TODO: Add eviction of clean sessions. Keep a linked hash set of clean session hashes and
    /// evict the oldest when its size exceeds a threshold. Or just evict everything on every flush
    /// (and accept more frequent database accesses).
    sessions: FnvHashMap<SessionHash, Session>,

    rand: SystemRandom,
}

impl State {
    pub fn init(conn: &Connection) -> Result<Self, Error> {
        let mut state = State {
            users_by_id: BTreeMap::new(),
            users_by_name: BTreeMap::new(),
            sessions: FnvHashMap::default(),
            rand: ring::rand::SystemRandom::new(),
        };
        let mut stmt = conn.prepare(
            r#"
            select
                id,
                username,
                config,
                password_hash,
                password_id,
                password_failure_count,
                permissions
            from
                user
            "#,
        )?;
        let mut rows = stmt.query(params![])?;
        while let Some(row) = rows.next()? {
            let id = row.get(0)?;
            let name: String = row.get(1)?;
            let mut permissions = Permissions::new();
            permissions.merge_from_bytes(row.get_ref(6)?.as_blob()?)?;
            state.users_by_id.insert(
                id,
                User {
                    id,
                    username: name.clone(),
                    config: row.get(2)?,
                    password_hash: row.get(3)?,
                    password_id: row.get(4)?,
                    password_failure_count: row.get(5)?,
                    dirty: false,
                    permissions,
                },
            );
            state.users_by_name.insert(name, id);
        }
        Ok(state)
    }

    pub fn apply(&mut self, conn: &Connection, change: UserChange) -> Result<&User, base::Error> {
        if let Some(id) = change.id {
            self.update_user(conn, id, change)
        } else {
            self.add_user(conn, change)
        }
    }

    pub fn users_by_id(&self) -> &BTreeMap<i32, User> {
        &self.users_by_id
    }

    pub fn get_user_by_id_mut(&mut self, id: i32) -> Option<&mut User> {
        self.users_by_id.get_mut(&id)
    }

    fn update_user(
        &mut self,
        conn: &Connection,
        id: i32,
        change: UserChange,
    ) -> Result<&User, base::Error> {
        let mut stmt = conn
            .prepare_cached(
                r#"
            update user
            set
                username = :username,
                password_hash = :password_hash,
                password_id = :password_id,
                password_failure_count = :password_failure_count,
                config = :config,
                permissions = :permissions
            where
                id = :id
            "#,
            )
            .context(ErrorKind::Unknown)?;
        let e = self.users_by_id.entry(id);
        let e = match e {
            ::std::collections::btree_map::Entry::Vacant(_) => panic!("missing uid {id}!"),
            ::std::collections::btree_map::Entry::Occupied(e) => e,
        };
        {
            let (phash, pid, pcount) = match change.set_password_hash.as_ref() {
                None => {
                    let u = e.get();
                    (&u.password_hash, u.password_id, u.password_failure_count)
                }
                Some(h) => (h, e.get().password_id + 1, 0),
            };
            let permissions = change
                .permissions
                .write_to_bytes()
                .expect("proto3->vec is infallible");
            stmt.execute(named_params! {
                ":username": &change.username[..],
                ":password_hash": phash,
                ":password_id": &pid,
                ":password_failure_count": &pcount,
                ":config": &change.config,
                ":id": &id,
                ":permissions": &permissions,
            })
            .context(ErrorKind::Unknown)?;
        }
        let u = e.into_mut();
        if u.username != change.username {
            self.users_by_name.remove(&u.username);
            self.users_by_name.insert(change.username.clone(), u.id);
            u.username = change.username;
        }
        if let Some(h) = change.set_password_hash {
            u.password_hash = h;
            u.password_id += 1;
            u.password_failure_count = 0;
        }
        u.config = change.config;
        u.permissions = change.permissions;
        Ok(u)
    }

    fn add_user(&mut self, conn: &Connection, change: UserChange) -> Result<&User, base::Error> {
        let mut stmt = conn
            .prepare_cached(
                r#"
            insert into user (username,  password_hash,  config,  permissions)
                      values (:username, :password_hash, :config, :permissions)
            "#,
            )
            .context(ErrorKind::Unknown)?;
        let password_hash = change.set_password_hash.unwrap_or(None);
        let permissions = change
            .permissions
            .write_to_bytes()
            .expect("proto3->vec is infallible");
        stmt.execute(named_params! {
            ":username": &change.username[..],
            ":password_hash": &password_hash,
            ":config": &change.config,
            ":permissions": &permissions,
        })
        .context(ErrorKind::Unknown)?;
        let id = conn.last_insert_rowid() as i32;
        self.users_by_name.insert(change.username.clone(), id);
        let e = self.users_by_id.entry(id);
        let e = match e {
            ::std::collections::btree_map::Entry::Vacant(e) => e,
            ::std::collections::btree_map::Entry::Occupied(_) => panic!("uid {id} conflict!"),
        };
        Ok(e.insert(User {
            id,
            username: change.username,
            config: change.config,
            password_hash,
            password_id: 0,
            password_failure_count: 0,
            dirty: false,
            permissions: change.permissions,
        }))
    }

    pub fn delete_user(&mut self, conn: &mut Connection, id: i32) -> Result<(), base::Error> {
        let tx = conn.transaction().context(ErrorKind::Unknown)?;
        tx.execute("delete from user_session where user_id = ?", params![id])
            .context(ErrorKind::Unknown)?;
        {
            let mut user_stmt = tx
                .prepare_cached("delete from user where id = ?")
                .context(ErrorKind::Unknown)?;
            if user_stmt.execute(params![id]).context(ErrorKind::Unknown)? != 1 {
                bail_t!(NotFound, "user {} not found", id);
            }
        }
        tx.commit().context(ErrorKind::Unknown)?;
        let name = self.users_by_id.remove(&id).unwrap().username;
        self.users_by_name
            .remove(&name)
            .expect("users_by_name should be consistent with users_by_id");
        self.sessions.retain(|_k, ref mut v| v.user_id != id);
        Ok(())
    }

    pub fn get_user(&self, username: &str) -> Option<&User> {
        self.users_by_name.get(username).map(|id| {
            self.users_by_id
                .get(id)
                .expect("users_by_name implies users_by_id")
        })
    }

    pub fn login_by_password(
        &mut self,
        conn: &Connection,
        req: Request,
        username: &str,
        password: String,
        domain: Option<Vec<u8>>,
        session_flags: i32,
    ) -> Result<(RawSessionId, &Session), Error> {
        let id = self
            .users_by_name
            .get(username)
            .ok_or_else(|| format_err!("no such user {:?}", username))?;
        let u = self
            .users_by_id
            .get_mut(id)
            .expect("users_by_name implies users_by_id");
        if u.config.disabled {
            bail!("user {:?} is disabled", username);
        }
        if !u.check_password(Some(&password))? {
            bail_t!(Unauthenticated, "incorrect password");
        }
        let password_id = u.password_id;
        State::make_session_int(
            &self.rand,
            conn,
            req,
            u,
            domain,
            Some(password_id),
            session_flags,
            &mut self.sessions,
            u.permissions.clone(),
        )
    }

    /// Makes a session directly (no password required).
    pub fn make_session<'s>(
        &'s mut self,
        conn: &Connection,
        creation: Request,
        uid: i32,
        domain: Option<Vec<u8>>,
        flags: i32,
        permissions: Permissions,
    ) -> Result<(RawSessionId, &'s Session), Error> {
        let u = self
            .users_by_id
            .get_mut(&uid)
            .ok_or_else(|| format_err!("no such uid {:?}", uid))?;
        if u.config.disabled {
            bail!("user is disabled");
        }
        State::make_session_int(
            &self.rand,
            conn,
            creation,
            u,
            domain,
            None,
            flags,
            &mut self.sessions,
            permissions,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn make_session_int<'s>(
        rand: &SystemRandom,
        conn: &Connection,
        creation: Request,
        user: &mut User,
        domain: Option<Vec<u8>>,
        creation_password_id: Option<i32>,
        flags: i32,
        sessions: &'s mut FnvHashMap<SessionHash, Session>,
        permissions: Permissions,
    ) -> Result<(RawSessionId, &'s Session), Error> {
        let mut session_id = RawSessionId([0u8; 48]);
        rand.fill(&mut session_id.0).unwrap();
        let mut seed = [0u8; 32];
        rand.fill(&mut seed).unwrap();
        let hash = session_id.hash();
        let mut stmt = conn.prepare_cached(
            r#"
            insert into user_session (session_id_hash,  user_id,  seed,  flags,  domain,
                                      creation_password_id,  creation_time_sec,
                                      creation_user_agent,  creation_peer_addr,
                                      permissions)
                              values (:session_id_hash, :user_id, :seed, :flags, :domain,
                                      :creation_password_id, :creation_time_sec,
                                      :creation_user_agent, :creation_peer_addr,
                                      :permissions)
            "#,
        )?;
        let addr = creation.addr_buf();
        let addr: Option<&[u8]> = addr.as_ref().map(|a| a.as_ref());
        let permissions_blob = permissions
            .write_to_bytes()
            .expect("proto3->vec is infallible");
        stmt.execute(named_params! {
            ":session_id_hash": &hash.0[..],
            ":user_id": &user.id,
            ":seed": &seed[..],
            ":flags": &flags,
            ":domain": &domain,
            ":creation_password_id": &creation_password_id,
            ":creation_time_sec": &creation.when_sec,
            ":creation_user_agent": &creation.user_agent,
            ":creation_peer_addr": &addr,
            ":permissions": &permissions_blob,
        })?;
        let e = match sessions.entry(hash) {
            ::std::collections::hash_map::Entry::Occupied(_) => panic!("duplicate session hash!"),
            ::std::collections::hash_map::Entry::Vacant(e) => e,
        };
        let session = e.insert(Session {
            user_id: user.id,
            flags,
            domain,
            creation_password_id,
            creation,
            seed: Seed(seed),
            permissions,
            ..Default::default()
        });
        Ok((session_id, session))
    }

    pub fn authenticate_session(
        &mut self,
        conn: &Connection,
        req: Request,
        hash: &SessionHash,
    ) -> Result<(&Session, &User), base::Error> {
        let s = match self.sessions.entry(*hash) {
            ::std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            ::std::collections::hash_map::Entry::Vacant(e) => {
                let s = lookup_session(conn, hash).map_err(|e| {
                    e.map(|k| match k {
                        ErrorKind::NotFound => ErrorKind::Unauthenticated,
                        e => e,
                    })
                })?;
                e.insert(s)
            }
        };
        let u = match self.users_by_id.get(&s.user_id) {
            None => bail_t!(Internal, "session references nonexistent user!"),
            Some(u) => u,
        };
        if let Some(r) = s.revocation_reason {
            bail_t!(Unauthenticated, "session is no longer valid (reason={})", r);
        }
        s.last_use = req;
        s.use_count += 1;
        s.dirty = true;
        if u.config.disabled {
            bail_t!(Unauthenticated, "user {:?} is disabled", &u.username);
        }
        Ok((s, u))
    }

    pub fn revoke_session(
        &mut self,
        conn: &Connection,
        reason: RevocationReason,
        detail: Option<String>,
        req: Request,
        hash: &SessionHash,
    ) -> Result<(), Error> {
        let s = match self.sessions.entry(*hash) {
            ::std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            ::std::collections::hash_map::Entry::Vacant(e) => e.insert(lookup_session(conn, hash)?),
        };
        if s.revocation_reason.is_none() {
            let mut stmt = conn.prepare(
                r#"
                update user_session
                set
                    revocation_time_sec = ?,
                    revocation_user_agent = ?,
                    revocation_peer_addr = ?,
                    revocation_reason = ?,
                    revocation_reason_detail = ?
                where
                    session_id_hash = ?
                "#,
            )?;
            let addr = req.addr_buf();
            let addr: Option<&[u8]> = addr.as_ref().map(|a| a.as_ref());
            stmt.execute(params![
                req.when_sec,
                req.user_agent,
                addr,
                reason as i32,
                detail,
                &hash.0[..],
            ])?;
            s.revocation = req;
            s.revocation_reason = Some(reason as i32);
        }
        Ok(())
    }

    /// Flushes all pending database changes to the given transaction.
    ///
    /// The caller is expected to call `post_flush` afterward if the transaction is
    /// successfully committed.
    pub fn flush(&self, tx: &Transaction) -> Result<(), Error> {
        let mut u_stmt = tx.prepare(
            r#"
            update user
            set
                password_failure_count = :password_failure_count,
                password_hash = :password_hash
            where
                id = :id
            "#,
        )?;
        let mut s_stmt = tx.prepare(
            r#"
            update user_session
            set
                last_use_time_sec = :last_use_time_sec,
                last_use_user_agent = :last_use_user_agent,
                last_use_peer_addr = :last_use_peer_addr,
                use_count = :use_count
            where
                session_id_hash = :hash
            "#,
        )?;
        for (&id, u) in &self.users_by_id {
            if !u.dirty {
                continue;
            }
            info!(
                "flushing user with hash: {}",
                u.password_hash.as_ref().unwrap()
            );
            u_stmt.execute(named_params! {
                ":password_failure_count": &u.password_failure_count,
                ":password_hash": &u.password_hash,
                ":id": &id,
            })?;
        }
        for s in self.sessions.values() {
            if !s.dirty {
                continue;
            }
            let addr = s.last_use.addr_buf();
            let addr: Option<&[u8]> = addr.as_ref().map(|a| a.as_ref());
            s_stmt.execute(named_params! {
                ":last_use_time_sec": &s.last_use.when_sec,
                ":last_use_user_agent": &s.last_use.user_agent,
                ":last_use_peer_addr": &addr,
                ":use_count": &s.use_count,
            })?;
        }
        Ok(())
    }

    /// Marks that the previous `flush` was completed successfully.
    ///
    /// See notes there.
    pub fn post_flush(&mut self) {
        for u in self.users_by_id.values_mut() {
            u.dirty = false;
        }
        for s in self.sessions.values_mut() {
            s.dirty = false;
        }
    }
}

fn lookup_session(conn: &Connection, hash: &SessionHash) -> Result<Session, base::Error> {
    let mut stmt = conn
        .prepare_cached(
            r#"
        select
            user_id,
            seed,
            flags,
            domain,
            description,
            creation_password_id,
            creation_time_sec,
            creation_user_agent,
            creation_peer_addr,
            revocation_time_sec,
            revocation_user_agent,
            revocation_peer_addr,
            revocation_reason,
            revocation_reason_detail,
            last_use_time_sec,
            last_use_user_agent,
            last_use_peer_addr,
            use_count,
            permissions
        from
            user_session
        where
            session_id_hash = ?
        "#,
        )
        .err_kind(ErrorKind::Internal)?;
    let mut rows = stmt
        .query(params![&hash.0[..]])
        .err_kind(ErrorKind::Internal)?;
    let row = rows
        .next()
        .err_kind(ErrorKind::Internal)?
        .ok_or_else(|| format_err_t!(NotFound, "no such session"))?;
    let creation_addr: FromSqlIpAddr = row.get(8).err_kind(ErrorKind::Internal)?;
    let revocation_addr: FromSqlIpAddr = row.get(11).err_kind(ErrorKind::Internal)?;
    let last_use_addr: FromSqlIpAddr = row.get(16).err_kind(ErrorKind::Internal)?;
    let mut permissions = Permissions::new();
    permissions
        .merge_from_bytes(
            row.get_ref(18)
                .err_kind(ErrorKind::Internal)?
                .as_blob()
                .err_kind(ErrorKind::Internal)?,
        )
        .err_kind(ErrorKind::Internal)?;
    Ok(Session {
        user_id: row.get(0).err_kind(ErrorKind::Internal)?,
        seed: row.get(1).err_kind(ErrorKind::Internal)?,
        flags: row.get(2).err_kind(ErrorKind::Internal)?,
        domain: row.get(3).err_kind(ErrorKind::Internal)?,
        description: row.get(4).err_kind(ErrorKind::Internal)?,
        creation_password_id: row.get(5).err_kind(ErrorKind::Internal)?,
        creation: Request {
            when_sec: row.get(6).err_kind(ErrorKind::Internal)?,
            user_agent: row.get(7).err_kind(ErrorKind::Internal)?,
            addr: creation_addr.0,
        },
        revocation: Request {
            when_sec: row.get(9).err_kind(ErrorKind::Internal)?,
            user_agent: row.get(10).err_kind(ErrorKind::Internal)?,
            addr: revocation_addr.0,
        },
        revocation_reason: row.get(12).err_kind(ErrorKind::Internal)?,
        revocation_reason_detail: row.get(13).err_kind(ErrorKind::Internal)?,
        last_use: Request {
            when_sec: row.get(14).err_kind(ErrorKind::Internal)?,
            user_agent: row.get(15).err_kind(ErrorKind::Internal)?,
            addr: last_use_addr.0,
        },
        use_count: row.get(17).err_kind(ErrorKind::Internal)?,
        dirty: false,
        permissions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::testutil;
    use rusqlite::Connection;

    #[test]
    fn open_empty_db() {
        testutil::init();
        set_test_config();
        let mut conn = Connection::open_in_memory().unwrap();
        db::init(&mut conn).unwrap();
        State::init(&conn).unwrap();
    }

    #[test]
    fn create_login_use_logout() {
        testutil::init();
        let mut conn = Connection::open_in_memory().unwrap();
        db::init(&mut conn).unwrap();
        let mut state = State::init(&conn).unwrap();
        let req = Request {
            when_sec: Some(42),
            addr: Some(::std::net::IpAddr::V4(::std::net::Ipv4Addr::new(
                127, 0, 0, 1,
            ))),
            user_agent: Some(b"some ua".to_vec()),
        };
        let (uid, mut c) = {
            let u = state
                .apply(&conn, UserChange::add_user("slamb".to_owned()))
                .unwrap();
            (u.id, u.change())
        };
        let e = state
            .login_by_password(
                &conn,
                req.clone(),
                "slamb",
                "hunter2".to_owned(),
                Some(b"nvr.example.com".to_vec()),
                0,
            )
            .unwrap_err();
        assert_eq!(format!("{e}"), "Unauthenticated: incorrect password");
        c.set_password("hunter2".to_owned());
        state.apply(&conn, c).unwrap();
        let e = state
            .login_by_password(
                &conn,
                req.clone(),
                "slamb",
                "hunter3".to_owned(),
                Some(b"nvr.example.com".to_vec()),
                0,
            )
            .unwrap_err();
        assert_eq!(format!("{e}"), "Unauthenticated: incorrect password");
        let sid = {
            let (sid, s) = state
                .login_by_password(
                    &conn,
                    req.clone(),
                    "slamb",
                    "hunter2".to_owned(),
                    Some(b"nvr.example.com".to_vec()),
                    0,
                )
                .unwrap();
            assert_eq!(s.user_id, uid);
            sid
        };

        {
            let (_, u) = state
                .authenticate_session(&conn, req.clone(), &sid.hash())
                .unwrap();
            assert_eq!(u.id, uid);
        }
        state
            .revoke_session(
                &conn,
                RevocationReason::LoggedOut,
                None,
                req.clone(),
                &sid.hash(),
            )
            .unwrap();
        let e = state
            .authenticate_session(&conn, req.clone(), &sid.hash())
            .unwrap_err();
        assert_eq!(
            format!("{e}"),
            "Unauthenticated: session is no longer valid (reason=1)"
        );

        // Everything should persist across reload.
        drop(state);
        let mut state = State::init(&conn).unwrap();
        let e = state
            .authenticate_session(&conn, req, &sid.hash())
            .unwrap_err();
        assert_eq!(
            format!("{e}"),
            "Unauthenticated: session is no longer valid (reason=1)"
        );
    }

    #[test]
    fn revoke_not_in_cache() {
        testutil::init();
        let mut conn = Connection::open_in_memory().unwrap();
        db::init(&mut conn).unwrap();
        let mut state = State::init(&conn).unwrap();
        let req = Request {
            when_sec: Some(42),
            addr: Some(::std::net::IpAddr::V4(::std::net::Ipv4Addr::new(
                127, 0, 0, 1,
            ))),
            user_agent: Some(b"some ua".to_vec()),
        };
        {
            let mut c = UserChange::add_user("slamb".to_owned());
            c.set_password("hunter2".to_owned());
            state.apply(&conn, c).unwrap();
        };
        let sid = state
            .login_by_password(
                &conn,
                req.clone(),
                "slamb",
                "hunter2".to_owned(),
                Some(b"nvr.example.com".to_vec()),
                0,
            )
            .unwrap()
            .0;
        state
            .authenticate_session(&conn, req.clone(), &sid.hash())
            .unwrap();

        // Reload.
        drop(state);
        let mut state = State::init(&conn).unwrap();
        state
            .revoke_session(
                &conn,
                RevocationReason::LoggedOut,
                None,
                req.clone(),
                &sid.hash(),
            )
            .unwrap();
        let e = state
            .authenticate_session(&conn, req, &sid.hash())
            .unwrap_err();
        assert_eq!(
            format!("{e}"),
            "Unauthenticated: session is no longer valid (reason=1)"
        );
    }

    #[test]
    fn disable() {
        testutil::init();
        let mut conn = Connection::open_in_memory().unwrap();
        db::init(&mut conn).unwrap();
        let mut state = State::init(&conn).unwrap();
        let req = Request {
            when_sec: Some(42),
            addr: Some(::std::net::IpAddr::V4(::std::net::Ipv4Addr::new(
                127, 0, 0, 1,
            ))),
            user_agent: Some(b"some ua".to_vec()),
        };
        let uid = {
            let mut c = UserChange::add_user("slamb".to_owned());
            c.set_password("hunter2".to_owned());
            state.apply(&conn, c).unwrap().id
        };

        // Get a session for later.
        let sid = state
            .login_by_password(
                &conn,
                req.clone(),
                "slamb",
                "hunter2".to_owned(),
                Some(b"nvr.example.com".to_vec()),
                0,
            )
            .unwrap()
            .0;

        // Disable the user.
        {
            let mut c = state.users_by_id().get(&uid).unwrap().change();
            c.config.disabled = true;
            state.apply(&conn, c).unwrap();
        }

        // Fresh logins shouldn't work.
        let e = state
            .login_by_password(
                &conn,
                req.clone(),
                "slamb",
                "hunter2".to_owned(),
                Some(b"nvr.example.com".to_vec()),
                0,
            )
            .unwrap_err();
        assert_eq!(format!("{e}"), "user \"slamb\" is disabled");

        // Authenticating existing sessions shouldn't work either.
        let e = state
            .authenticate_session(&conn, req.clone(), &sid.hash())
            .unwrap_err();
        assert_eq!(
            format!("{e}"),
            "Unauthenticated: user \"slamb\" is disabled"
        );

        // The user should still be disabled after reload.
        drop(state);
        let mut state = State::init(&conn).unwrap();
        let e = state
            .authenticate_session(&conn, req, &sid.hash())
            .unwrap_err();
        assert_eq!(
            format!("{e}"),
            "Unauthenticated: user \"slamb\" is disabled"
        );
    }

    #[test]
    fn change() {
        testutil::init();
        let mut conn = Connection::open_in_memory().unwrap();
        db::init(&mut conn).unwrap();
        let mut state = State::init(&conn).unwrap();
        let uid = {
            let mut c = UserChange::add_user("slamb".to_owned());
            c.set_password("hunter2".to_owned());
            state.apply(&conn, c).unwrap().id
        };

        let user = state.users_by_id().get(&uid).unwrap();
        let mut c = user.change();
        c.username = "foo".to_owned();
        state.apply(&conn, c).unwrap();

        assert!(state.users_by_name.get("slamb").is_none());
        assert!(state.users_by_name.get("foo").is_some());
    }

    #[test]
    fn delete() {
        testutil::init();
        let mut conn = Connection::open_in_memory().unwrap();
        db::init(&mut conn).unwrap();
        let mut state = State::init(&conn).unwrap();
        let req = Request {
            when_sec: Some(42),
            addr: Some(::std::net::IpAddr::V4(::std::net::Ipv4Addr::new(
                127, 0, 0, 1,
            ))),
            user_agent: Some(b"some ua".to_vec()),
        };
        let uid = {
            let mut c = UserChange::add_user("slamb".to_owned());
            c.set_password("hunter2".to_owned());
            state.apply(&conn, c).unwrap().id
        };

        // Get a session for later.
        let (sid, _) = state
            .login_by_password(
                &conn,
                req.clone(),
                "slamb",
                "hunter2".to_owned(),
                Some(b"nvr.example.com".to_vec()),
                0,
            )
            .unwrap();

        state.delete_user(&mut conn, uid).unwrap();
        assert!(state.users_by_id().get(&uid).is_none());
        let e = state
            .authenticate_session(&conn, req.clone(), &sid.hash())
            .unwrap_err();
        assert_eq!(format!("{e}"), "Unauthenticated: no such session");

        // The user should still be deleted after reload.
        drop(state);
        let mut state = State::init(&conn).unwrap();
        assert!(state.users_by_id().get(&uid).is_none());
        let e = state
            .authenticate_session(&conn, req, &sid.hash())
            .unwrap_err();
        assert_eq!(format!("{e}"), "Unauthenticated: no such session");
    }

    #[test]
    fn permissions() {
        testutil::init();
        let mut conn = Connection::open_in_memory().unwrap();
        db::init(&mut conn).unwrap();
        let mut state = State::init(&conn).unwrap();
        let mut change = UserChange::add_user("slamb".to_owned());
        change.permissions.view_video = true;
        let u = state.apply(&conn, change).unwrap();
        assert!(u.permissions.view_video);
        assert!(!u.permissions.update_signals);
        let mut change = u.change();
        assert!(change.permissions.view_video);
        assert!(!change.permissions.update_signals);
        change.permissions.update_signals = true;
        let u = state.apply(&conn, change).unwrap();
        assert!(u.permissions.view_video);
        assert!(u.permissions.update_signals);
        let uid = u.id;

        {
            let tx = conn.transaction().unwrap();
            state.flush(&tx).unwrap();
            tx.commit().unwrap();
        }
        let state = State::init(&conn).unwrap();
        let u = state.users_by_id().get(&uid).unwrap();
        assert!(u.permissions.view_video);
        assert!(u.permissions.update_signals);
    }

    #[test]
    fn preferences() {
        testutil::init();
        let mut conn = Connection::open_in_memory().unwrap();
        db::init(&mut conn).unwrap();
        let mut state = State::init(&conn).unwrap();
        let mut change = UserChange::add_user("slamb".to_owned());
        change
            .config
            .preferences
            .insert("foo".to_string(), 42.into());
        let u = state.apply(&conn, change).unwrap();
        let mut change = u.change();
        change
            .config
            .preferences
            .insert("bar".to_string(), 26.into());
        let u = state.apply(&conn, change).unwrap();
        assert_eq!(u.config.preferences.get("foo"), Some(&42.into()));
        assert_eq!(u.config.preferences.get("bar"), Some(&26.into()));
        let uid = u.id;

        {
            let tx = conn.transaction().unwrap();
            state.flush(&tx).unwrap();
            tx.commit().unwrap();
        }
        let state = State::init(&conn).unwrap();
        let u = state.users_by_id().get(&uid).unwrap();
        assert_eq!(u.config.preferences.get("foo"), Some(&42.into()));
        assert_eq!(u.config.preferences.get("bar"), Some(&26.into()));
    }
}
