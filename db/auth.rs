// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2018 The Moonfire NVR Authors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// In addition, as a special exception, the copyright holders give
// permission to link the code of portions of this program with the
// OpenSSL library under certain conditions as described in each
// individual source file, and distribute linked combinations including
// the two.
//
// You must obey the GNU General Public License in all respects for all
// of the code used other than OpenSSL. If you modify file(s) with this
// exception, you may extend this exception to your version of the
// file(s), but you are not obligated to do so. If you do not wish to do
// so, delete this exception statement from your version. If you delete
// this exception statement from all source files in the program, then
// also delete it here.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

use log::info;
use base::strutil;
use blake2_rfc::blake2b::blake2b;
use crate::schema::Permissions;
use failure::{Error, bail, format_err};
use fnv::FnvHashMap;
use lazy_static::lazy_static;
use libpasta;
use parking_lot::Mutex;
use protobuf::Message;
use rusqlite::{Connection, Transaction, params};
use std::collections::BTreeMap;
use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;

lazy_static! {
    static ref PASTA_CONFIG: Mutex<Arc<libpasta::Config>> =
        Mutex::new(Arc::new(libpasta::Config::default()));
}

/// For testing only: use a fast but insecure libpasta config.
/// See also <https://github.com/libpasta/libpasta/issues/9>.
/// Call via `testutil::init()`.
pub(crate) fn set_test_config() {
    *PASTA_CONFIG.lock() =
        Arc::new(libpasta::Config::with_primitive(libpasta::primitives::Bcrypt::new(2)));
}

enum UserFlag {
    Disabled = 1,
}

#[derive(Debug)]
pub struct User {
    pub id: i32,
    pub username: String,
    pub flags: i32,
    password_hash: Option<String>,
    pub password_id: i32,
    pub password_failure_count: i64,
    pub unix_uid: Option<i32>,
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
            flags: self.flags,
            set_password_hash: None,
            unix_uid: self.unix_uid,
            permissions: self.permissions.clone(),
        }
    }

    pub fn has_password(&self) -> bool { self.password_hash.is_some() }
    fn disabled(&self) -> bool { (self.flags & UserFlag::Disabled as i32) != 0 }
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
    pub flags: i32,
    set_password_hash: Option<Option<String>>,
    pub unix_uid: Option<i32>,
    pub permissions: Permissions,
}

impl UserChange {
    pub fn add_user(username: String) -> Self {
        UserChange {
            id: None,
            username,
            flags: 0,
            set_password_hash: None,
            unix_uid: None,
            permissions: Permissions::default(),
        }
    }

    pub fn set_password(&mut self, pwd: String) {
        let c = Arc::clone(&PASTA_CONFIG.lock());
        self.set_password_hash = Some(Some(c.hash_password(&pwd)));
    }

    pub fn clear_password(&mut self) {
        self.set_password_hash = Some(None);
    }

    pub fn disable(&mut self) {
        self.flags |= UserFlag::Disabled as i32;
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
            ValueRef::Blob(ref b) => {
                match b.len() {
                    4 => {
                        let mut buf = [0u8; 4];
                        buf.copy_from_slice(b);
                        Ok(FromSqlIpAddr(Some(buf.into())))
                    },
                    16 => {
                        let mut buf = [0u8; 16];
                        buf.copy_from_slice(b);
                        Ok(FromSqlIpAddr(Some(buf.into())))
                    },
                    _ => Err(rusqlite::types::FromSqlError::InvalidType),
                }
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
}

#[derive(Debug, Default)]
pub struct Session {
    user_id: i32,
    flags: i32,  // bitmask of SessionFlag enum values
    domain: Option<Vec<u8>>,
    description: Option<String>,
    seed: Seed,

    creation_password_id: Option<i32>,
    creation: Request,

    revocation: Request,
    revocation_reason: Option<i32>,  // see RevocationReason enum
    revocation_reason_detail: Option<String>,

    pub permissions: Permissions,

    last_use: Request,
    use_count: i32,
    dirty: bool,
}

impl Session {
    pub fn csrf(&self) -> SessionHash {
        let r = blake2b(24, b"csrf", &self.seed.0[..]);
        let mut h = SessionHash([0u8; 24]);
        h.0.copy_from_slice(r.as_bytes());
        h
    }
}

/// A raw session id (not base64-encoded). Sensitive. Never stored in the database.
pub struct RawSessionId([u8; 48]);

impl RawSessionId {
    pub fn new() -> Self { RawSessionId([0u8; 48]) }

    pub fn decode_base64(input: &[u8]) -> Result<Self, Error> {
        let mut s = RawSessionId::new();
        let l = ::base64::decode_config_slice(input, ::base64::STANDARD_NO_PAD, &mut s.0[..])?;
        if l != 48 {
            bail!("session id must be 48 bytes");
        }
        Ok(s)
    }

    pub fn hash(&self) -> SessionHash {
        let r = blake2b(24, &[], &self.0[..]);
        let mut h = SessionHash([0u8; 24]);
        h.0.copy_from_slice(r.as_bytes());
        h
    }
}

impl AsRef<[u8]> for RawSessionId {
    fn as_ref(&self) -> &[u8] { &self.0[..] }
}

impl AsMut<[u8]> for RawSessionId {
    fn as_mut(&mut self) -> &mut [u8] { &mut self.0[..] }
}

impl fmt::Debug for RawSessionId {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(f, "RawSessionId(\"{}\")", &strutil::hex(&self.0[..]))
    }
}

/// A Blake2b-256 (48 bytes) of data associated with the session.
/// This is currently used in two ways:
/// *   the csrf token is a blake2b drived from the session's seed. This is put into the `sc`
///     cookie.
/// *   the 48-byte session id is hashed to be used as a database key.
#[derive(Copy, Clone, Default, PartialEq, Eq, Hash)]
pub struct SessionHash(pub [u8; 24]);

impl SessionHash {
    pub fn encode_base64(&self, output: &mut [u8; 32]) {
        ::base64::encode_config_slice(&self.0, ::base64::STANDARD_NO_PAD, output);
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
        write!(f, "SessionHash(\"{}\")", ::std::str::from_utf8(&buf[..]).expect("base64 is UTF-8"))
    }
}

#[derive(Copy, Clone, Debug, Default)]
struct Seed([u8; 32]);

impl rusqlite::types::FromSql for Seed {
    fn column_result(value: rusqlite::types::ValueRef) -> rusqlite::types::FromSqlResult<Self> {
        let b = value.as_blob()?;
        if b.len() != 32 {
            return Err(rusqlite::types::FromSqlError::Other(
                Box::new(format_err!("expected a 32-byte seed").compat())));
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
}

impl State {
    pub fn init(conn: &Connection) -> Result<Self, Error> {
        let mut state = State {
            users_by_id: BTreeMap::new(),
            users_by_name: BTreeMap::new(),
            sessions: FnvHashMap::default(),
        };
        let mut stmt = conn.prepare(r#"
            select
                id,
                username,
                flags,
                password_hash,
                password_id,
                password_failure_count,
                unix_uid,
                permissions
            from
                user
        "#)?;
        let mut rows = stmt.query(params![])?;
        while let Some(row) = rows.next()? {
            let id = row.get(0)?;
            let name: String = row.get(1)?;
            let mut permissions = Permissions::new();
            permissions.merge_from_bytes(row.get_raw_checked(7)?.as_blob()?)?;
            state.users_by_id.insert(id, User {
                id,
                username: name.clone(),
                flags: row.get(2)?,
                password_hash: row.get(3)?,
                password_id: row.get(4)?,
                password_failure_count: row.get(5)?,
                unix_uid: row.get(6)?,
                dirty: false,
                permissions,
            });
            state.users_by_name.insert(name, id);
        }
        Ok(state)
    }

    pub fn apply(&mut self, conn: &Connection, change: UserChange) -> Result<&User, Error> {
        if let Some(id) = change.id {
            self.update_user(conn, id, change)
        } else {
            self.add_user(conn, change)
        }
    }

    pub fn users_by_id(&self) -> &BTreeMap<i32, User> { &self.users_by_id }

    fn update_user(&mut self, conn: &Connection, id: i32, change: UserChange)
                   -> Result<&User, Error> {
        let mut stmt = conn.prepare_cached(r#"
            update user
            set
                username = :username,
                password_hash = :password_hash,
                password_id = :password_id,
                password_failure_count = :password_failure_count,
                flags = :flags,
                unix_uid = :unix_uid,
                permissions = :permissions
            where
                id = :id
        "#)?;
        let e = self.users_by_id.entry(id);
        let e = match e {
            ::std::collections::btree_map::Entry::Vacant(_) => panic!("missing uid {}!", id),
            ::std::collections::btree_map::Entry::Occupied(e) => e,
        };
        {
            let (phash, pid, pcount) = match change.set_password_hash.as_ref() {
                None => {
                    let u = e.get();
                    (&u.password_hash, u.password_id, u.password_failure_count)
                },
                Some(h) => (h, e.get().password_id + 1, 0),
            };
            let permissions = change.permissions.write_to_bytes().expect("proto3->vec is infallible");
            stmt.execute_named(&[
                (":username", &&change.username[..]),
                (":password_hash", phash),
                (":password_id", &pid),
                (":password_failure_count", &pcount),
                (":flags", &change.flags),
                (":unix_uid", &change.unix_uid),
                (":id", &id),
                (":permissions", &permissions),
            ])?;
        }
        let u = e.into_mut();
        u.username = change.username;
        if let Some(h) = change.set_password_hash {
            u.password_hash = h;
            u.password_id += 1;
            u.password_failure_count = 0;
        }
        u.flags = change.flags;
        u.unix_uid = change.unix_uid;
        u.permissions = change.permissions;
        Ok(u)
    }

    fn add_user(&mut self, conn: &Connection, change: UserChange) -> Result<&User, Error> {
        let mut stmt = conn.prepare_cached(r#"
            insert into user (username,  password_hash,  flags,  unix_uid,  permissions)
                      values (:username, :password_hash, :flags, :unix_uid, :permissions)
        "#)?;
        let password_hash = change.set_password_hash.unwrap_or(None);
        let permissions = change.permissions.write_to_bytes().expect("proto3->vec is infallible");
        stmt.execute_named(&[
            (":username", &&change.username[..]),
            (":password_hash", &password_hash),
            (":flags", &change.flags),
            (":unix_uid", &change.unix_uid),
            (":permissions", &permissions),
        ])?;
        let id = conn.last_insert_rowid() as i32;
        self.users_by_name.insert(change.username.clone(), id);
        let e = self.users_by_id.entry(id);
        let e = match e {
            ::std::collections::btree_map::Entry::Vacant(e) => e,
            ::std::collections::btree_map::Entry::Occupied(_) => panic!("uid {} conflict!", id),
        };
        Ok(e.insert(User {
            id,
            username: change.username,
            flags: change.flags,
            password_hash,
            password_id: 0,
            password_failure_count: 0,
            unix_uid: change.unix_uid,
            dirty: false,
            permissions: change.permissions,
        }))
    }

    pub fn delete_user(&mut self, conn: &mut Connection, id: i32) -> Result<(), Error> {
        let tx = conn.transaction()?;
        tx.execute("delete from user_session where user_id = ?", params![id])?;
        {
            let mut user_stmt = tx.prepare_cached("delete from user where id = ?")?;
            if user_stmt.execute(params![id])? != 1 {
                bail!("user {} not found", id);
            }
        }
        tx.commit()?;
        let name = self.users_by_id.remove(&id).unwrap().username;
        self.users_by_name.remove(&name).unwrap();
        self.sessions.retain(|_k, ref mut v| v.user_id != id);
        Ok(())
    }

    pub fn get_user(&self, username: &str) -> Option<&User> {
        self.users_by_name
            .get(username)
            .map(|id| self.users_by_id.get(id).expect("users_by_name implies users_by_id"))
    }

    pub fn login_by_password(&mut self, conn: &Connection, req: Request, username: &str,
                             password: String, domain: Option<Vec<u8>>, session_flags: i32)
                             -> Result<(RawSessionId, &Session), Error> {
        let id = self.users_by_name.get(username)
            .ok_or_else(|| format_err!("no such user {:?}", username))?;
        let u = self.users_by_id.get_mut(id).expect("users_by_name implies users_by_id");
        if u.disabled() {
            bail!("user {:?} is disabled", username);
        }
        let new_hash = {
            let hash = match u.password_hash.as_ref() {
                None => bail!("no password set for user {:?}", username),
                Some(h) => h,
            };
            let c = Arc::clone(&PASTA_CONFIG.lock());
            match c.verify_password_update_hash(hash, &password) {
                libpasta::HashUpdate::Failed => {
                    u.dirty = true;
                    u.password_failure_count += 1;
                    bail!("incorrect password for user {:?}", username);
                },
                libpasta::HashUpdate::Verified(new_pwd) => new_pwd,
            }
        };
        if let Some(h) = new_hash {
            u.password_hash = Some(h);
            u.dirty = true;
        }
        let password_id = u.password_id;
        State::make_session_int(conn, req, u, domain, Some(password_id), session_flags,
                            &mut self.sessions, u.permissions.clone())
    }

    /// Makes a session directly (no password required).
    pub fn make_session<'s>(&'s mut self, conn: &Connection, creation: Request, uid: i32,
                            domain: Option<Vec<u8>>, flags: i32, permissions: Permissions)
                            -> Result<(RawSessionId, &'s Session), Error> {
        let u = self.users_by_id.get_mut(&uid).ok_or_else(|| format_err!("no such uid {:?}", uid))?;
        if u.disabled() {
            bail!("user is disabled");
        }
        State::make_session_int(conn, creation, u, domain, None, flags, &mut self.sessions,
                                permissions)
    }

    fn make_session_int<'s>(conn: &Connection, creation: Request, user: &mut User,
                            domain: Option<Vec<u8>>, creation_password_id: Option<i32>, flags: i32,
                            sessions: &'s mut FnvHashMap<SessionHash, Session>,
                            permissions: Permissions)
                            -> Result<(RawSessionId, &'s Session), Error> {
        let mut session_id = RawSessionId::new();
        ::openssl::rand::rand_bytes(&mut session_id.0).unwrap();
        let mut seed = [0u8; 32];
        ::openssl::rand::rand_bytes(&mut seed).unwrap();
        let hash = session_id.hash();
        let mut stmt = conn.prepare_cached(r#"
            insert into user_session (session_id_hash,  user_id,  seed,  flags,  domain,
                                      creation_password_id,  creation_time_sec,
                                      creation_user_agent,  creation_peer_addr,
                                      permissions)
                              values (:session_id_hash, :user_id, :seed, :flags, :domain,
                                      :creation_password_id, :creation_time_sec,
                                      :creation_user_agent, :creation_peer_addr,
                                      :permissions)
        "#)?;
        let addr = creation.addr_buf();
        let addr: Option<&[u8]> = addr.as_ref().map(|a| a.as_ref());
        let permissions_blob = permissions.write_to_bytes().expect("proto3->vec is infallible");
        stmt.execute_named(&[
            (":session_id_hash", &&hash.0[..]),
            (":user_id", &user.id),
            (":seed", &&seed[..]),
            (":flags", &flags),
            (":domain", &domain),
            (":creation_password_id", &creation_password_id),
            (":creation_time_sec", &creation.when_sec),
            (":creation_user_agent", &creation.user_agent),
            (":creation_peer_addr", &addr),
            (":permissions", &permissions_blob),
        ])?;
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

    pub fn authenticate_session(&mut self, conn: &Connection, req: Request, hash: &SessionHash)
                                -> Result<(&Session, &User), Error> {
        let s = match self.sessions.entry(*hash) {
            ::std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            ::std::collections::hash_map::Entry::Vacant(e) => e.insert(lookup_session(conn, hash)?),
        };
        let u = match self.users_by_id.get(&s.user_id) {
            None => bail!("session references nonexistent user!"),
            Some(u) => u,
        };
        if let Some(r) = s.revocation_reason {
            bail!("session is no longer valid (reason={})", r);
        }
        s.last_use = req;
        s.use_count += 1;
        s.dirty = true;
        if u.disabled() {
            bail!("user {:?} is disabled", &u.username);
        }
        Ok((s, u))
    }

    pub fn revoke_session(&mut self, conn: &Connection, reason: RevocationReason,
                          detail: Option<String>, req: Request, hash: &SessionHash)
                          -> Result<(), Error> {
        let s = match self.sessions.entry(*hash) {
            ::std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            ::std::collections::hash_map::Entry::Vacant(e) => e.insert(lookup_session(conn, hash)?),
        };
        if s.revocation_reason.is_none() {
            let mut stmt = conn.prepare(r#"
                update user_session
                set
                    revocation_time_sec = ?,
                    revocation_user_agent = ?,
                    revocation_peer_addr = ?,
                    revocation_reason = ?,
                    revocation_reason_detail = ?
                where
                    session_id_hash = ?
            "#)?;
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
        let mut u_stmt = tx.prepare(r#"
            update user
            set
                password_failure_count = :password_failure_count,
                password_hash = :password_hash
            where
                id = :id
        "#)?;
        let mut s_stmt = tx.prepare(r#"
            update user_session
            set
                last_use_time_sec = :last_use_time_sec,
                last_use_user_agent = :last_use_user_agent,
                last_use_peer_addr = :last_use_peer_addr,
                use_count = :use_count
            where
                session_id_hash = :hash
        "#)?;
        for (&id, u) in &self.users_by_id {
            if !u.dirty {
                continue;
            }
            info!("flushing user with hash: {}", u.password_hash.as_ref().unwrap());
            u_stmt.execute_named(&[
                (":password_failure_count", &u.password_failure_count),
                (":password_hash", &u.password_hash),
                (":id", &id),
            ])?;
        }
        for (_, s) in &self.sessions {
            if !s.dirty {
                continue;
            }
            let addr = s.last_use.addr_buf();
            let addr: Option<&[u8]> = addr.as_ref().map(|a| a.as_ref());
            s_stmt.execute_named(&[
                (":last_use_time_sec", &s.last_use.when_sec),
                (":last_use_user_agent", &s.last_use.user_agent),
                (":last_use_peer_addr", &addr),
                (":use_count", &s.use_count),
            ])?;
        }
        Ok(())
    }

    /// Marks that the previous `flush` was completed successfully.
    ///
    /// See notes there.
    pub fn post_flush(&mut self) {
        for (_, u) in &mut self.users_by_id {
            u.dirty = false;
        }
        for (_, s) in &mut self.sessions {
            s.dirty = false;
        }
    }
}

fn lookup_session(conn: &Connection, hash: &SessionHash) -> Result<Session, Error> {
    let mut stmt = conn.prepare_cached(r#"
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
    "#)?;
    let mut rows = stmt.query(params![&hash.0[..]])?;
    let row = rows.next()?.ok_or_else(|| format_err!("no such session"))?;
    let creation_addr: FromSqlIpAddr = row.get(8)?;
    let revocation_addr: FromSqlIpAddr = row.get(11)?;
    let last_use_addr: FromSqlIpAddr = row.get(16)?;
    let mut permissions = Permissions::new();
    permissions.merge_from_bytes(row.get_raw_checked(18)?.as_blob()?)?;
    Ok(Session {
        user_id: row.get(0)?,
        seed: row.get(1)?,
        flags: row.get(2)?,
        domain: row.get(3)?,
        description: row.get(4)?,
        creation_password_id: row.get(5)?,
        creation: Request {
            when_sec: row.get(6)?,
            user_agent: row.get(7)?,
            addr: creation_addr.0,
        },
        revocation: Request {
            when_sec: row.get(9)?,
            user_agent: row.get(10)?,
            addr: revocation_addr.0,
        },
        revocation_reason: row.get(12)?,
        revocation_reason_detail: row.get(13)?,
        last_use: Request {
            when_sec: row.get(14)?,
            user_agent: row.get(15)?,
            addr: last_use_addr.0,
        },
        use_count: row.get(17)?,
        dirty: false,
        permissions,
    })
}

#[cfg(test)]
mod tests {
    use crate::db;
    use rusqlite::Connection;
    use super::*;
    use crate::testutil;

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
            addr: Some(::std::net::IpAddr::V4(::std::net::Ipv4Addr::new(127, 0, 0, 1))),
            user_agent: Some(b"some ua".to_vec()),
        };
        let (uid, mut c) = {
            let u = state.apply(&conn, UserChange::add_user("slamb".to_owned())).unwrap();
            (u.id, u.change())
        };
        let e = state.login_by_password(&conn, req.clone(), "slamb", "hunter2".to_owned(),
                                        Some(b"nvr.example.com".to_vec()), 0).unwrap_err();
        assert_eq!(format!("{}", e), "no password set for user \"slamb\"");
        c.set_password("hunter2".to_owned());
        state.apply(&conn, c).unwrap();
        let e = state.login_by_password(&conn, req.clone(), "slamb",
                                       "hunter3".to_owned(),
                                       Some(b"nvr.example.com".to_vec()), 0).unwrap_err();
        assert_eq!(format!("{}", e), "incorrect password for user \"slamb\"");
        let sid = {
            let (sid, s) = state.login_by_password(&conn, req.clone(), "slamb",
                                                  "hunter2".to_owned(),
                                                  Some(b"nvr.example.com".to_vec()), 0).unwrap();
            assert_eq!(s.user_id, uid);
            sid
        };

        {
            let (_, u) = state.authenticate_session(&conn, req.clone(), &sid.hash()).unwrap();
            assert_eq!(u.id, uid);
        }
        state.revoke_session(&conn, RevocationReason::LoggedOut, None, req.clone(),
                             &sid.hash()).unwrap();
        let e = state.authenticate_session(&conn, req.clone(), &sid.hash()).unwrap_err();
        assert_eq!(format!("{}", e), "session is no longer valid (reason=1)");

        // Everything should persist across reload.
        drop(state);
        let mut state = State::init(&conn).unwrap();
        let e = state.authenticate_session(&conn, req, &sid.hash()).unwrap_err();
        assert_eq!(format!("{}", e), "session is no longer valid (reason=1)");
    }

    #[test]
    fn revoke_not_in_cache() {
        testutil::init();
        let mut conn = Connection::open_in_memory().unwrap();
        db::init(&mut conn).unwrap();
        let mut state = State::init(&conn).unwrap();
        let req = Request {
            when_sec: Some(42),
            addr: Some(::std::net::IpAddr::V4(::std::net::Ipv4Addr::new(127, 0, 0, 1))),
            user_agent: Some(b"some ua".to_vec()),
        };
        {
            let mut c = UserChange::add_user("slamb".to_owned());
            c.set_password("hunter2".to_owned());
            state.apply(&conn, c).unwrap();
        };
        let sid = state.login_by_password(&conn, req.clone(), "slamb",
                                          "hunter2".to_owned(),
                                          Some(b"nvr.example.com".to_vec()), 0).unwrap().0;
        state.authenticate_session(&conn, req.clone(), &sid.hash()).unwrap();

        // Reload.
        drop(state);
        let mut state = State::init(&conn).unwrap();
        state.revoke_session(&conn, RevocationReason::LoggedOut, None, req.clone(),
                             &sid.hash()).unwrap();
        let e = state.authenticate_session(&conn, req, &sid.hash()).unwrap_err();
        assert_eq!(format!("{}", e), "session is no longer valid (reason=1)");
    }

    #[test]
    fn upgrade_hash() {
        // This hash is generated with cost=1 vs the cost=2 of PASTA_CONFIG.
        let insecure_hash =
            libpasta::Config::with_primitive(libpasta::primitives::Bcrypt::new(1))
            .hash_password("hunter2");
        testutil::init();
        let mut conn = Connection::open_in_memory().unwrap();
        db::init(&mut conn).unwrap();
        let mut state = State::init(&conn).unwrap();
        let mut change = UserChange::add_user("slamb".to_owned());

        // hunter2, in insecure MD5.
        change.set_password_hash = Some(Some(insecure_hash.clone()));
        let uid = {
            let u = state.apply(&conn, change).unwrap();
            assert_eq!(&insecure_hash, u.password_hash.as_ref().unwrap());
            u.id
        };

        let req = Request {
            when_sec: Some(42),
            addr: Some(::std::net::IpAddr::V4(::std::net::Ipv4Addr::new(127, 0, 0, 1))),
            user_agent: Some(b"some ua".to_vec()),
        };
        state.login_by_password(&conn, req.clone(), "slamb", "hunter2".to_owned(),
                                Some(b"nvr.example.com".to_vec()), 0).unwrap();
        let new_hash = {
            // Password should have been automatically upgraded.
            let u = state.users_by_id().get(&uid).unwrap();
            assert!(u.dirty);
            assert_ne!(u.password_hash.as_ref().unwrap(), &insecure_hash);
            u.password_hash.as_ref().unwrap().clone()
        };

        {
            let tx = conn.transaction().unwrap();
            state.flush(&tx).unwrap();
            tx.commit().unwrap();
        }

        // On reload, the new hash should still be visible.
        drop(state);
        let mut state = State::init(&conn).unwrap();
        {
            let u = state.users_by_id().get(&uid).unwrap();
            assert!(!u.dirty);
            assert_eq!(u.password_hash.as_ref().unwrap(), &new_hash);
        }

        // Login should still work.
        state.login_by_password(&conn, req.clone(), "slamb", "hunter2".to_owned(),
                                Some(b"nvr.example.com".to_vec()), 0).unwrap();
    }

    #[test]
    fn disable() {
        testutil::init();
        let mut conn = Connection::open_in_memory().unwrap();
        db::init(&mut conn).unwrap();
        let mut state = State::init(&conn).unwrap();
        let req = Request {
            when_sec: Some(42),
            addr: Some(::std::net::IpAddr::V4(::std::net::Ipv4Addr::new(127, 0, 0, 1))),
            user_agent: Some(b"some ua".to_vec()),
        };
        let uid = {
            let mut c = UserChange::add_user("slamb".to_owned());
            c.set_password("hunter2".to_owned());
            state.apply(&conn, c).unwrap().id
        };

        // Get a session for later.
        let sid = state.login_by_password(&conn, req.clone(), "slamb",
                                          "hunter2".to_owned(),
                                          Some(b"nvr.example.com".to_vec()), 0).unwrap().0;

        // Disable the user.
        {
            let mut c = state.users_by_id().get(&uid).unwrap().change();
            c.disable();
            state.apply(&conn, c).unwrap();
        }

        // Fresh logins shouldn't work.
        let e = state.login_by_password(&conn, req.clone(), "slamb",
                                       "hunter2".to_owned(),
                                       Some(b"nvr.example.com".to_vec()), 0).unwrap_err();
        assert_eq!(format!("{}", e), "user \"slamb\" is disabled");

        // Authenticating existing sessions shouldn't work either.
        let e = state.authenticate_session(&conn, req.clone(), &sid.hash()).unwrap_err();
        assert_eq!(format!("{}", e), "user \"slamb\" is disabled");

        // The user should still be disabled after reload.
        drop(state);
        let mut state = State::init(&conn).unwrap();
        let e = state.authenticate_session(&conn, req, &sid.hash()).unwrap_err();
        assert_eq!(format!("{}", e), "user \"slamb\" is disabled");
    }

    #[test]
    fn delete() {
        testutil::init();
        let mut conn = Connection::open_in_memory().unwrap();
        db::init(&mut conn).unwrap();
        let mut state = State::init(&conn).unwrap();
        let req = Request {
            when_sec: Some(42),
            addr: Some(::std::net::IpAddr::V4(::std::net::Ipv4Addr::new(127, 0, 0, 1))),
            user_agent: Some(b"some ua".to_vec()),
        };
        let uid = {
            let mut c = UserChange::add_user("slamb".to_owned());
            c.set_password("hunter2".to_owned());
            state.apply(&conn, c).unwrap().id
        };

        // Get a session for later.
        let (sid, _) = state.login_by_password(&conn, req.clone(), "slamb",
                                               "hunter2".to_owned(),
                                               Some(b"nvr.example.com".to_vec()), 0).unwrap();

        state.delete_user(&mut conn, uid).unwrap();
        assert!(state.users_by_id().get(&uid).is_none());
        let e = state.authenticate_session(&conn, req.clone(), &sid.hash()).unwrap_err();
        assert_eq!(format!("{}", e), "no such session");

        // The user should still be deleted after reload.
        drop(state);
        let mut state = State::init(&conn).unwrap();
        assert!(state.users_by_id().get(&uid).is_none());
        let e = state.authenticate_session(&conn, req.clone(), &sid.hash()).unwrap_err();
        assert_eq!(format!("{}", e), "no such session");
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
}
