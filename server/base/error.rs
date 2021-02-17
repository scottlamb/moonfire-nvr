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

use failure::{Backtrace, Context, Fail};
use std::fmt::{self, Write};

/// Returns a pretty-and-informative version of `e`.
pub fn prettify_failure(e: &failure::Error) -> String {
    let mut msg = e.to_string();
    for cause in e.iter_causes() {
        write!(&mut msg, "\ncaused by: {}", cause).unwrap();
    }
    if e.backtrace().is_empty() {
        write!(
            &mut msg,
            "\n\n(set environment variable RUST_BACKTRACE=1 to see backtraces)"
        )
        .unwrap();
    } else {
        write!(&mut msg, "\n\nBacktrace:\n{}", e.backtrace()).unwrap();
    }
    msg
}

#[derive(Debug)]
pub struct Error {
    inner: Context<ErrorKind>,
}

impl Error {
    pub fn kind(&self) -> ErrorKind {
        *self.inner.get_context()
    }

    pub fn compat(self) -> failure::Compat<Context<ErrorKind>> {
        self.inner.compat()
    }
}

impl Fail for Error {
    fn cause(&self) -> Option<&dyn Fail> {
        self.inner.cause()
    }

    fn backtrace(&self) -> Option<&Backtrace> {
        self.inner.backtrace()
    }
}

impl From<ErrorKind> for Error {
    fn from(kind: ErrorKind) -> Error {
        Error {
            inner: Context::new(kind),
        }
    }
}

impl From<Context<ErrorKind>> for Error {
    fn from(inner: Context<ErrorKind>) -> Error {
        Error { inner }
    }
}

/*impl From<failure::Error> for Error {
    fn from(e: failure::Error) -> Error {
        Error { inner: e.context(ErrorKind::Unknown) }
    }
}

impl<E: std::error::Error + Send + Sync + 'static> From<E> for Error {
    fn from(e: E) -> Error {
        let f = e as Fail;
        Error { inner: f.context(ErrorKind::Unknown) }
    }
}*/

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.inner.cause() {
            None => fmt::Display::fmt(&self.kind(), f),
            Some(c) => write!(f, "{}: {}", self.kind(), c),
        }
    }
}

/// Error kind.
///
/// These codes are taken from
/// [grpc::StatusCode](https://github.com/grpc/grpc/blob/0e00c430827e81d61e1e7164ef04ca21ccbfaa77/include/grpcpp/impl/codegen/status_code_enum.h),
/// which is a nice general-purpose classification of errors. See that link for descriptions of
/// each error.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Fail)]
#[non_exhaustive]
#[rustfmt::skip]
pub enum ErrorKind {
    #[fail(display = "Cancelled")] Cancelled,
    #[fail(display = "Unknown")] Unknown,
    #[fail(display = "Invalid argument")] InvalidArgument,
    #[fail(display = "Deadline exceeded")] DeadlineExceeded,
    #[fail(display = "Not found")] NotFound,
    #[fail(display = "Already exists")] AlreadyExists,
    #[fail(display = "Permission denied")] PermissionDenied,
    #[fail(display = "Unauthenticated")] Unauthenticated,
    #[fail(display = "Resource exhausted")] ResourceExhausted,
    #[fail(display = "Failed precondition")] FailedPrecondition,
    #[fail(display = "Aborted")] Aborted,
    #[fail(display = "Out of range")] OutOfRange,
    #[fail(display = "Unimplemented")] Unimplemented,
    #[fail(display = "Internal")] Internal,
    #[fail(display = "Unavailable")] Unavailable,
    #[fail(display = "Data loss")] DataLoss,
}

/// Extension methods for `Result`.
pub trait ResultExt<T, E> {
    /// Annotates an error with the given kind.
    /// Example:
    /// ```
    /// use moonfire_base::{ErrorKind, ResultExt};
    /// use std::io::Read;
    /// let mut buf = [0u8; 1];
    /// let r = std::io::Cursor::new("").read_exact(&mut buf[..]).err_kind(ErrorKind::Internal);
    /// assert_eq!(r.unwrap_err().kind(), ErrorKind::Internal);
    /// ```
    fn err_kind(self, k: ErrorKind) -> Result<T, Error>;
}

impl<T, E> ResultExt<T, E> for Result<T, E>
where
    E: Into<failure::Error>,
{
    fn err_kind(self, k: ErrorKind) -> Result<T, Error> {
        self.map_err(|e| e.into().context(k).into())
    }
}

/// Like `failure::bail!`, but the first argument specifies a type as an `ErrorKind`.
///
/// Example:
/// ```
/// use moonfire_base::bail_t;
/// let e = || -> Result<(), moonfire_base::Error> {
///     bail_t!(Unauthenticated, "unknown user: {}", "slamb");
/// }().unwrap_err();
/// assert_eq!(e.kind(), moonfire_base::ErrorKind::Unauthenticated);
/// assert_eq!(e.to_string(), "Unauthenticated: unknown user: slamb");
/// ```
#[macro_export]
macro_rules! bail_t {
    ($t:ident, $e:expr) => {
        return Err(failure::err_msg($e).context($crate::ErrorKind::$t).into());
    };
    ($t:ident, $fmt:expr, $($arg:tt)+) => {
        return Err(failure::err_msg(format!($fmt, $($arg)+)).context($crate::ErrorKind::$t).into());
    };
}

/// Like `failure::format_err!`, but the first argument specifies a type as an `ErrorKind`.
///
/// Example:
/// ```
/// use moonfire_base::format_err_t;
/// let e = format_err_t!(Unauthenticated, "unknown user: {}", "slamb");
/// assert_eq!(e.kind(), moonfire_base::ErrorKind::Unauthenticated);
/// assert_eq!(e.to_string(), "Unauthenticated: unknown user: slamb");
/// ```
#[macro_export]
macro_rules! format_err_t {
    ($t:ident, $e:expr) => {
        Into::<$crate::Error>::into(failure::err_msg($e).context($crate::ErrorKind::$t))
    };
    ($t:ident, $fmt:expr, $($arg:tt)+) => {
        Into::<$crate::Error>::into(failure::err_msg(format!($fmt, $($arg)+))
                                    .context($crate::ErrorKind::$t))
    };
}
