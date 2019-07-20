// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2019 Scott Lamb <slamb@slamb.org>
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

//! Subcommand to login a user (without requiring a password).

use base::clock::{self, Clocks};
use db::auth::SessionFlags;
use failure::{Error, ResultExt, bail, format_err};
use serde::Deserialize;
use std::os::unix::fs::OpenOptionsExt as _;
use std::io::Write as _;
use std::path::PathBuf;

static USAGE: &'static str = r#"
Logs in a user, returning the session cookie.

This is a privileged command that directly accesses the database. It doesn't
check the user's password and even can be used to create sessions with
permissions the user doesn't have.

Usage:

    moonfire-nvr login [options] <username>
    moonfire-nvr login --help

Options:

    --db-dir=DIR     Set the directory holding the SQLite3 index database. This
                     is typically on a flash device.
                     [default: /var/lib/moonfire-nvr/db]
    --permissions=PERMISSIONS
                     Create a session with the given permissions. If
                     unspecified, uses user's default permissions.
    --domain=DOMAIN  The domain this cookie lives on. Optional.
    --curl-cookie-jar=FILE
                     Writes the cookie to a new curl-compatible cookie-jar
                     file. --domain must be specified. This can be used later
                     with curl's --cookie flag.
    --session-flags=FLAGS
                     Set the given db::auth::SessionFlags.
                     [default: http-only,secure,same-site,same-site-strict]
"#;

#[derive(Debug, Default, Deserialize, Eq, PartialEq)]
struct Args {
    flag_db_dir: String,
    flag_permissions: Option<String>,
    flag_domain: Option<String>,
    flag_curl_cookie_jar: Option<PathBuf>,
    flag_session_flags: String,
    arg_username: String,
}

pub fn run() -> Result<(), Error> {
    let args: Args = super::parse_args(USAGE)?;
    let clocks = clock::RealClocks {};
    let (_db_dir, conn) = super::open_conn(&args.flag_db_dir, super::OpenMode::ReadWrite)?;
    let db = std::sync::Arc::new(db::Database::new(clocks.clone(), conn, true).unwrap());
    let mut l = db.lock();
    let u = l.get_user(&args.arg_username)
        .ok_or_else(|| format_err!("no such user {:?}", &args.arg_username))?;
    let permissions = match args.flag_permissions {
        None => u.permissions.clone(),
        Some(s) => protobuf::text_format::parse_from_str(&s)
                   .context("unable to parse --permissions")?
    };
    let creation = db::auth::Request {
        when_sec: Some(db.clocks().realtime().sec),
        user_agent: None,
        addr: None,
    };
    let mut flags = 0;
    for f in args.flag_session_flags.split(',') {
        flags |= match f {
            "http-only"        => SessionFlags::HttpOnly,
            "secure"           => SessionFlags::Secure,
            "same-site"        => SessionFlags::SameSite,
            "same-site-strict" => SessionFlags::SameSiteStrict,
            _ => bail!("unknown session flag {:?}", f),
        } as i32;
    }
    let uid = u.id;
    drop(u);
    let (sid, _) = l.make_session(creation, uid,
                                  args.flag_domain.as_ref().map(|d| d.as_bytes().to_owned()),
                                  flags, permissions)?;
    let mut encoded = [0u8; 64];
    base64::encode_config_slice(&sid, base64::STANDARD_NO_PAD, &mut encoded);
    let encoded = std::str::from_utf8(&encoded[..]).expect("base64 is valid UTF-8");

    if let Some(ref p) = args.flag_curl_cookie_jar {
        let d = args.flag_domain.as_ref()
                    .ok_or_else(|| format_err!("--cookiejar requires --domain"))?;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(p)
            .map_err(|e| format_err!("Unable to open {}: {}", p.display(), e))?;
        write!(&mut f,
               "# Netscape HTTP Cookie File\n\
               # https://curl.haxx.se/docs/http-cookies.html\n\
               # This file was generated by moonfire-nvr login! Edit at your own risk.\n\n\
               {}\n", curl_cookie(encoded, flags, d))?;
        f.sync_all()?;
        println!("Wrote cookie to {}", p.display());
    } else {
        println!("s={}", encoded);
    }
    Ok(())
}

fn curl_cookie(cookie: &str, flags: i32, domain: &str) -> String {
    format!("{httponly}{domain}\t{tailmatch}\t{path}\t{secure}\t{expires}\t{name}\t{value}",
            httponly=if (flags & SessionFlags::HttpOnly as i32) != 0 { "#HttpOnly_" } else { "" },
            domain=domain,
            tailmatch="FALSE",
            path="/",
            secure=if (flags & SessionFlags::Secure as i32) != 0 { "TRUE" } else { "FALSE" },
            expires="9223372036854775807",  // 64-bit CURL_OFF_T_MAX, never expires
            name="s",
            value=cookie)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_args() {
        let args: Args = docopt::Docopt::new(USAGE).unwrap()
            .argv(&["nvr", "login", "--curl-cookie-jar=foo.txt", "slamb"])
            .deserialize().unwrap();
        assert_eq!(args, Args {
            flag_db_dir: "/var/lib/moonfire-nvr/db".to_owned(),
            flag_curl_cookie_jar: Some(PathBuf::from("foo.txt")),
            flag_session_flags: "http-only,secure,same-site,same-site-strict".to_owned(),
            arg_username: "slamb".to_owned(),
            ..Default::default()
        });
    }

    #[test]
    fn test_curl_cookie() {
        assert_eq!(curl_cookie("o3mx3OntO7GzwwsD54OuyQ4IuipYrwPR2aiULPHSudAa+xIhwWjb+w1TnGRh8Z5Q",
                               SessionFlags::HttpOnly as i32, "localhost"),
                   "#HttpOnly_localhost\tFALSE\t/\tFALSE\t9223372036854775807\ts\t\
                   o3mx3OntO7GzwwsD54OuyQ4IuipYrwPR2aiULPHSudAa+xIhwWjb+w1TnGRh8Z5Q");
    }
}
