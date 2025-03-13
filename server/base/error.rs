// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2018 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

use std::backtrace::Backtrace;
use std::error::Error as StdError;
use std::fmt::{Debug, Display};
//use std::num::NonZeroU16;

pub use coded::ErrorKind;
use rc_box::ArcBox;
use std::sync::Arc;

/// Like [`coded::ToErrKind`] but with more third-party implementations.
///
/// It's not possible to implement those here on that trait because of the orphan rule.
pub trait ToErrKind {
    fn err_kind(&self) -> ErrorKind;
}

impl ToErrKind for Error {
    #[inline]
    fn err_kind(&self) -> ErrorKind {
        self.0.kind
    }
}

impl ToErrKind for std::io::Error {
    #[inline]
    fn err_kind(&self) -> ErrorKind {
        self.kind().into()
    }
}

impl ToErrKind for rusqlite::ErrorCode {
    fn err_kind(&self) -> ErrorKind {
        use rusqlite::ErrorCode;
        // https://www.sqlite.org/rescode.html
        match self {
            ErrorCode::InternalMalfunction => ErrorKind::Internal,
            ErrorCode::PermissionDenied => ErrorKind::PermissionDenied,
            ErrorCode::OperationAborted => ErrorKind::Aborted,

            // Conflict with another database connection in a process which is accessing
            // the database, apparently without using Moonfire NVR's scheme of acquiring
            // a lock on the db directory.
            // https://www.sqlite.org/wal.html#sometimes_queries_return_sqlite_busy_in_wal_mode
            ErrorCode::DatabaseBusy => ErrorKind::Unavailable,

            // Conflict within the same database connection. Shouldn't happen for Moonfire.
            ErrorCode::DatabaseLocked => ErrorKind::Internal,
            ErrorCode::OutOfMemory => ErrorKind::ResourceExhausted,
            ErrorCode::ReadOnly => ErrorKind::FailedPrecondition,
            ErrorCode::OperationInterrupted => ErrorKind::Aborted,
            ErrorCode::SystemIoFailure => ErrorKind::Unavailable,
            ErrorCode::DatabaseCorrupt => ErrorKind::DataLoss,
            ErrorCode::NotFound => ErrorKind::NotFound,
            ErrorCode::DiskFull => ErrorKind::ResourceExhausted,
            ErrorCode::CannotOpen => ErrorKind::Unavailable,

            // Similar to DatabaseBusy in this implies a conflict with another conn.
            ErrorCode::FileLockingProtocolFailed => ErrorKind::Unavailable,

            // Likewise: Moonfire NVR should never change the schema
            // mid-statement, so the most plausible explanation for
            // SchemaChange is another process.
            ErrorCode::SchemaChanged => ErrorKind::Unavailable,

            ErrorCode::TooBig => ErrorKind::ResourceExhausted,
            ErrorCode::ConstraintViolation => ErrorKind::Internal,
            ErrorCode::TypeMismatch => ErrorKind::Internal,
            ErrorCode::ApiMisuse => ErrorKind::Internal,
            ErrorCode::NoLargeFileSupport => ErrorKind::ResourceExhausted,
            ErrorCode::AuthorizationForStatementDenied => ErrorKind::Internal,
            ErrorCode::ParameterOutOfRange => ErrorKind::Internal,
            ErrorCode::NotADatabase => ErrorKind::FailedPrecondition,
            _ => ErrorKind::Unknown,
        }
    }
}

impl ToErrKind for rusqlite::Error {
    #[inline]
    fn err_kind(&self) -> ErrorKind {
        match self {
            rusqlite::Error::SqliteFailure(e, _) => e.code.err_kind(),
            _ => ErrorKind::Unknown,
        }
    }
}

impl ToErrKind for rusqlite::types::FromSqlError {
    fn err_kind(&self) -> ErrorKind {
        match self {
            rusqlite::types::FromSqlError::InvalidType => ErrorKind::FailedPrecondition,
            rusqlite::types::FromSqlError::OutOfRange(_) => ErrorKind::OutOfRange,
            rusqlite::types::FromSqlError::InvalidBlobSize { .. } => ErrorKind::OutOfRange,
            /* rusqlite::types::FromSqlError::Other(_) | */ _ => ErrorKind::Unknown,
        }
    }
}

impl ToErrKind for nix::Error {
    fn err_kind(&self) -> ErrorKind {
        use nix::Error;
        match *self {
            Error::EACCES | Error::EPERM => ErrorKind::PermissionDenied,
            Error::EDQUOT => ErrorKind::ResourceExhausted,
            Error::EBUSY
            | Error::EEXIST
            | Error::ENOTDIR
            | Error::EROFS
            | Error::EFBIG
            | Error::EOVERFLOW
            | Error::ENXIO
            | Error::ETXTBSY => ErrorKind::FailedPrecondition,
            Error::EINVAL | Error::ENAMETOOLONG => ErrorKind::InvalidArgument,
            Error::ELOOP => ErrorKind::FailedPrecondition,
            Error::EMLINK | Error::ENOMEM | Error::ENOSPC | Error::EMFILE | Error::ENFILE => {
                ErrorKind::ResourceExhausted
            }
            Error::EBADF | Error::EFAULT => ErrorKind::InvalidArgument,
            Error::EINTR | Error::EAGAIN => ErrorKind::Aborted,
            Error::ENOENT | Error::ENODEV => ErrorKind::NotFound,
            Error::EOPNOTSUPP => ErrorKind::Unimplemented,
            _ => ErrorKind::Unknown,
        }
    }
}

#[derive(Clone)]
pub struct Error(Arc<ErrorInner>);

struct ErrorInner {
    kind: ErrorKind,
    msg: Option<String>,
    //http_status: Option<NonZeroU16>,
    backtrace: Option<Backtrace>,
    source: Option<Box<dyn StdError + Sync + Send>>,
}

pub struct ErrorBuilder(rc_box::ArcBox<ErrorInner>);

impl Default for ErrorBuilder {
    #[inline]
    fn default() -> Self {
        Self(ArcBox::new(ErrorInner {
            kind: ErrorKind::Unknown,
            msg: None,
            // http_status: None,
            backtrace: None,
            source: None,
        }))
    }
}

impl From<ErrorKind> for ErrorBuilder {
    #[inline]
    fn from(value: ErrorKind) -> Self {
        Self::default().kind(value)
    }
}

impl ErrorBuilder {
    #[inline]
    pub fn kind(mut self, kind: ErrorKind) -> Self {
        self.0.kind = kind;
        self
    }

    #[inline]
    pub fn map<F: Fn(ErrorKind) -> ErrorKind>(mut self, f: F) -> Self {
        self.0.kind = f(self.0.kind);
        self
    }

    #[inline]
    pub fn msg(mut self, msg: String) -> Self {
        self.0.msg = Some(msg);
        self
    }

    #[inline]
    pub fn source<S: Into<Box<dyn StdError + Send + Sync + 'static>>>(mut self, source: S) -> Self {
        self.0.source = Some(source.into());
        self
    }

    #[inline]
    pub fn build(self) -> Error {
        Error(self.0.into())
    }

    #[inline]
    pub fn boxed(self) -> Box<dyn StdError + Send + Sync + 'static> {
        Box::new(ArcBox::into_inner(self.0))
    }
}

macro_rules! cvt {
    ($t:ty) => {
        impl From<$t> for ErrorBuilder {
            #[inline]
            fn from(t: $t) -> Self {
                Self::default().kind(ToErrKind::err_kind(&t)).source(t)
            }
        }
        impl From<$t> for Error {
            #[inline(always)]
            fn from(t: $t) -> Self {
                Self($crate::ErrorBuilder::from(t).0.into())
            }
        }
    };
}
cvt!(rusqlite::Error);
cvt!(rusqlite::types::FromSqlError);
cvt!(std::io::Error);
cvt!(nix::Error);

impl From<Error> for ErrorBuilder {
    #[inline]
    fn from(value: Error) -> Self {
        Self::default().kind(value.kind()).source(value)
    }
}

impl From<ErrorBuilder> for Error {
    #[inline]
    fn from(value: ErrorBuilder) -> Self {
        Self(value.0.into())
    }
}

/// Captures a backtrace if enabled for the given error kind.
// TODO: make this more configurable at runtime.
fn maybe_backtrace(kind: ErrorKind) -> Option<Backtrace> {
    if matches!(kind, ErrorKind::Internal | ErrorKind::Unknown) {
        Some(Backtrace::capture())
    } else {
        None
    }
}

impl Error {
    #[inline]
    pub fn wrap<E: StdError + Sync + Send + 'static>(kind: ErrorKind, e: E) -> Self {
        Self(Arc::new(ErrorInner {
            kind,
            msg: None,
            // http_status: None,
            backtrace: maybe_backtrace(kind),
            source: Some(Box::new(e)),
        }))
    }

    #[inline]
    pub fn kind(&self) -> ErrorKind {
        self.0.kind
    }

    #[inline]
    pub fn msg(&self) -> Option<&str> {
        self.0.msg.as_deref()
    }

    /// Returns a borrowed value which can display not only this error but also
    /// the full chain of causes and (where applicable) the stack trace.
    ///
    /// The exact format may change. Currently, it displays the stack trace for
    /// the current error but not any of the sources.
    #[inline]
    pub fn chain(&self) -> impl Display + '_ {
        ErrorChain(&self.0)
    }

    #[inline]
    pub fn boxed(self) -> Box<dyn StdError + Send + Sync + 'static> {
        Box::new(self)
    }
}

/// Formats this error alone (*not* its full chain).
impl Display for Error {
    #[inline(always)]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl Display for ErrorBuilder {
    #[inline(always)]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl Display for ErrorInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.msg {
            None => std::fmt::Display::fmt(self.kind.grpc_name(), f)?,
            Some(ref msg) => write!(f, "{}: {}", self.kind.grpc_name(), msg)?,
        }
        if let Some(ref bt) = self.backtrace {
            // TODO: only with "alternate"/# modifier?
            // Shorten this, maybe by switching to `backtrace` + using
            // `backtrace_ext::short_frames_strict` or similar.
            write!(f, "\nBacktrace:\n{}", bt)?;
        }
        Ok(())
    }
}

impl Debug for ErrorInner {
    #[inline(always)]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&ErrorChain(self), f)
    }
}

impl Debug for Error {
    #[inline(always)]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.0, f)
    }
}

impl Debug for ErrorBuilder {
    #[inline(always)]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.0, f)
    }
}

/// Value returned by [`Error::chain`].
struct ErrorChain<'a>(&'a ErrorInner);

impl Display for ErrorChain<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(self.0, f)?;
        let mut source = self.0.source();
        while let Some(n) = source {
            write!(f, "\ncaused by: {}", n)?;
            source = n.source()
        }
        Ok(())
    }
}

impl StdError for ErrorInner {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        // https://users.rust-lang.org/t/question-about-error-source-s-static-return-type/34515/8
        self.source.as_ref().map(|e| e.as_ref() as &_)
    }
}

impl StdError for Error {
    #[inline(always)]
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.source()
    }
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
    E: StdError + Sync + Send + 'static,
{
    fn err_kind(self, k: ErrorKind) -> Result<T, Error> {
        self.map_err(|e| ErrorBuilder::default().kind(k).source(e).build())
    }
}

/// Wrapper around `err!` which returns the error.
///
/// Example with positional arguments:
/// ```
/// use moonfire_base::bail;
/// let e = || -> Result<(), moonfire_base::Error> {
///     bail!(Unauthenticated, msg("unknown user: {}", "slamb"));
/// }().unwrap_err();
/// assert_eq!(e.kind(), moonfire_base::ErrorKind::Unauthenticated);
/// assert_eq!(e.to_string(), "UNAUTHENTICATED: unknown user: slamb");
/// ```
///
/// Example with named arguments:
/// ```
/// use moonfire_base::bail;
/// let e = || -> Result<(), moonfire_base::Error> {
///     let user = "slamb";
///     bail!(Unauthenticated, msg("unknown user: {user}"));
/// }().unwrap_err();
/// assert_eq!(e.kind(), moonfire_base::ErrorKind::Unauthenticated);
/// assert_eq!(e.to_string(), "UNAUTHENTICATED: unknown user: slamb");
/// ```
#[macro_export]
macro_rules! bail {
    ($($arg:tt)+) => {
        return Err($crate::err!($($arg)+).into());
    };
}

/// Constructs an [`Error`], tersely.
///
/// This is a shorthand way to use [`ErrorBuilder`].
///
/// The first argument is an `Into<ErrorBuilder>`, such as the following:
///
/// *   an [`ErrorKind`] enum variant name like `Unauthenticated`.
///     There's an implicit `use ::coded::ErrorKind::*` to allow the bare
///     variant names just within this restrictive scope where you're unlikely
///     to have conflicts with other identifiers.
/// *   an [`std::io::Error`] as a source, which sets the new `Error`'s
///     `ErrorKind` based on the `std::io::Error`.
/// *   an `Error` as a source, which similarly copies the `ErrorKind`.
/// *   an existing `ErrorBuilder`, which does not create a new source link.
///
/// Following arguments may be of these forms:
///
/// *   `msg(...)`, which expands to `.msg(format!(...))`. See [`ErrorBuilder::msg`].
/// *   `source(...)`, which simply expands to `.source($src)`. See [`ErrorBuilder::source`].
///
/// ## Examples
///
/// Simplest:
///
/// ```rust
/// # use coded::err;
/// let e = err!(InvalidArgument);
/// let e = err!(InvalidArgument,); // trailing commas are allowed
/// assert_eq!(e.kind(), coded::ErrorKind::InvalidArgument);
/// ```
///
/// Constructing with a fixed error variant name:
///
/// ```rust
/// # use {coded::err, std::error::Error, std::num::ParseIntError};
/// let input = "a12";
/// let src = i32::from_str_radix(input, 10).unwrap_err();
///
/// let e = err!(InvalidArgument, source(src.clone()), msg("bad argument {:?}", input));
/// // The line above is equivalent to:
/// let e2 = ::coded::ErrorBuilder::from(::coded::ErrorKind::InvalidArgument)
///     .source(src.clone())
///     .msg(format!("bad argument {:?}", input))
///     .build();
///
/// assert_eq!(e.kind(), coded::ErrorKind::InvalidArgument);
/// assert_eq!(e.source().unwrap().downcast_ref::<ParseIntError>().unwrap(), &src);
/// ```
///
/// Constructing from an `std::io::Error`:
///
/// ```rust
/// # use coded::err;
/// let e = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
/// let e = err!(e, msg("path {} not found", "foo"));
/// assert_eq!(e.kind(), coded::ErrorKind::NotFound);
/// ```
#[macro_export]
macro_rules! err {
    // This uses the "incremental TT munchers", "internal rules", and "push-down accumulation"
    // patterns explained in the excellent "The Little Book of Rust Macros":
    // <https://veykril.github.io/tlborm/decl-macros/patterns/push-down-acc.html>.

    (@accum $body:tt $(,)?) => {
        $body
    };

    (@accum ($($body:tt)*), source($src:expr) $($tail:tt)*) => {
        $crate::err!(@accum ($($body)*.source($src)) $($tail)*)
    };

    // msg(...) uses the `format!` form even when there's only the format string.
    // This can catch errors (e.g. https://github.com/dtolnay/anyhow/issues/55)
    // and will allow supporting implicit named parameters:
    // https://rust-lang.github.io/rfcs/2795-format-args-implicit-identifiers.html
    (@accum ($($body:tt)*), msg($format:expr) $($tail:tt)*) => {
        $crate::err!(@accum ($($body)*.msg(format!($format))) $($tail)*)
    };
    (@accum ($($body:tt)*), msg($format:expr, $($args:tt)*) $($tail:tt)*) => {
        $crate::err!(@accum ($($body)*.msg(format!($format, $($args)*))) $($tail)*)
    };

    ($builder:expr $(, $($tail:tt)*)? ) => {
        $crate::err!(@accum ({
                use $crate::ErrorKind::*;
                $crate::ErrorBuilder::from($builder)
            })
            , $($($tail)*)*
        )
    };
}
