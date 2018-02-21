// This file is part of Moonfire NVR, a security camera digital video recorder.
// Copyright (C) 2018 Scott Lamb <slamb@slamb.org>
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

#![cfg_attr(all(feature="nightly", test), feature(test))]

#[macro_use] extern crate failure;
extern crate fnv;
#[macro_use] extern crate lazy_static;
extern crate libc;
#[macro_use] extern crate log;
extern crate lru_cache;
extern crate mylog;
extern crate openssl;
extern crate parking_lot;
extern crate protobuf;
extern crate regex;
extern crate rusqlite;
extern crate time;
extern crate uuid;

mod coding;
pub mod db;
pub mod dir;
pub mod recording;
pub mod schema;

// This is only for #[cfg(test)], but it's also used by the dependent crate, and it appears that
// #[cfg(test)] is not passed on to dependencies.
pub mod testutil;

pub use db::*;
