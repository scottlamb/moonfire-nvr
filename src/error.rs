// This file is part of Moonfire NVR, a security camera digital video recorder.
// Copyright (C) 2016 Scott Lamb <slamb@slamb.org>
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

extern crate rusqlite;
extern crate time;
extern crate uuid;

use core::ops::Deref;
use core::num;
use ffmpeg;
use openssl::error::ErrorStack;
use serde_json;
use std::boxed::Box;
use std::convert::From;
use std::error;
use std::error::Error as E;
use std::fmt;
use std::io;
use std::result;
use std::string::String;

#[derive(Debug)]
pub struct Error {
    pub description: String,
    pub cause: Option<Box<error::Error + Send + Sync>>,
}

impl Error {
    pub fn new(description: String) -> Self {
        Error{description: description, cause: None }
    }
}

pub trait ResultExt<T> {
    /// Returns a new `Result` like this one except that errors are of type `Error` and annotated
    /// with the given prefix.
    fn annotate_err(self, prefix: &'static str) -> Result<T>;
}

impl<T, E> ResultExt<T> for result::Result<T, E> where E: 'static + error::Error + Send + Sync {
    fn annotate_err(self, prefix: &'static str) -> Result<T> {
        self.map_err(|e| Error{
            description: format!("{}: {}", prefix, e.description()),
            cause: Some(Box::new(e)),
        })
    }
}

impl error::Error for Error {
    fn description(&self) -> &str { &self.description }
    fn cause(&self) -> Option<&error::Error> {
        match self.cause {
            Some(ref b) => Some(b.deref()),
            None => None
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> result::Result<(), fmt::Error> {
        write!(f, "Error: {}\ncause: {:?}", self.description, self.cause)
    }
}

// TODO(slamb): isn't there a "<? implements error::Error>" or some such?

impl From<rusqlite::Error> for Error {
    fn from(err: rusqlite::Error) -> Self {
        Error{description: String::from(err.description()), cause: Some(Box::new(err))}
    }
}

impl From<fmt::Error> for Error {
    fn from(err: fmt::Error) -> Self {
        Error{description: String::from(err.description()), cause: Some(Box::new(err))}
    }
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Error{description: String::from(err.description()), cause: Some(Box::new(err))}
    }
}

impl From<time::ParseError> for Error {
    fn from(err: time::ParseError) -> Self {
        Error{description: String::from(err.description()), cause: Some(Box::new(err))}
    }
}

impl From<num::ParseIntError> for Error {
    fn from(err: num::ParseIntError) -> Self {
        Error{description: err.description().to_owned(), cause: Some(Box::new(err))}
    }
}

impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self {
        Error{description: format!("{} ({})", err.description(), err), cause: Some(Box::new(err))}
    }
}

impl From<ffmpeg::Error> for Error {
    fn from(err: ffmpeg::Error) -> Self {
        Error{description: format!("{} ({})", err.description(), err), cause: Some(Box::new(err))}
    }
}

impl From<uuid::ParseError> for Error {
    fn from(_: uuid::ParseError) -> Self {
        Error{description: String::from("UUID parse error"), cause: None}
    }
}

impl From<ErrorStack> for Error {
    fn from(_: ErrorStack) -> Self {
        Error{description: String::from("openssl error"), cause: None}
    }
}

pub type Result<T> = result::Result<T, Error>;
