// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2020 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Time and durations for Moonfire NVR's internal format.

use crate::{bail, err, Error};
use nom::branch::alt;
use nom::bytes::complete::{tag, take_while_m_n};
use nom::combinator::{map, map_res, opt};
use nom::sequence::{preceded, tuple};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops;
use std::str::FromStr;

use super::clock::SystemTime;

type IResult<'a, I, O> = nom::IResult<I, O, nom::error::VerboseError<&'a str>>;

pub const TIME_UNITS_PER_SEC: i64 = 90_000;

/// The zone to use for all time handling.
///
/// In normal operation this is assigned from `jiff::tz::TimeZone::system()` at
/// startup, but tests set it to a known political time zone instead.
///
/// Note that while fresh calls to `jiff::tz::TimeZone::system()` might return
/// new values, this time zone is fixed for the entire run. This is important
/// for `moonfire_db::days::Map`, where it's expected that adding values and
/// then later subtracting them will cancel out.
static GLOBAL_ZONE: std::sync::OnceLock<jiff::tz::TimeZone> = std::sync::OnceLock::new();

pub fn init_zone<F: FnOnce() -> jiff::tz::TimeZone>(f: F) {
    GLOBAL_ZONE.get_or_init(f);
}

pub fn global_zone() -> jiff::tz::TimeZone {
    GLOBAL_ZONE
        .get()
        .expect("global zone should be initialized")
        .clone()
}

/// A time specified as 90,000ths of a second since 1970-01-01 00:00:00 UTC.
#[derive(Clone, Copy, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Time(pub i64);

/// Returns a parser for a `len`-digit non-negative number which fits into `T`.
fn fixed_len_num<'a, T: FromStr>(len: usize) -> impl FnMut(&'a str) -> IResult<'a, &'a str, T> {
    map_res(
        take_while_m_n(len, len, |c: char| c.is_ascii_digit()),
        |input: &str| input.parse(),
    )
}

/// Parses `YYYY-mm-dd` into pieces.
fn parse_datepart(input: &str) -> IResult<&str, (i16, i8, i8)> {
    tuple((
        fixed_len_num(4),
        preceded(tag("-"), fixed_len_num(2)),
        preceded(tag("-"), fixed_len_num(2)),
    ))(input)
}

/// Parses `HH:MM[:SS[:FFFFF]]` into pieces.
fn parse_timepart(input: &str) -> IResult<&str, (i8, i8, i8, i32)> {
    let (input, (hr, _, min)) = tuple((fixed_len_num(2), tag(":"), fixed_len_num(2)))(input)?;
    let (input, stuff) = opt(tuple((
        preceded(tag(":"), fixed_len_num(2)),
        opt(preceded(tag(":"), fixed_len_num(5))),
    )))(input)?;
    let (sec, opt_subsec) = stuff.unwrap_or((0, None));
    Ok((input, (hr, min, sec, opt_subsec.unwrap_or(0))))
}

/// Parses `Z` (UTC) or `{+,-,}HH:MM` into a time zone offset in seconds.
fn parse_zone(input: &str) -> IResult<&str, i32> {
    alt((
        nom::combinator::value(0, tag("Z")),
        map(
            tuple((
                opt(nom::character::complete::one_of(&b"+-"[..])),
                fixed_len_num::<i32>(2),
                tag(":"),
                fixed_len_num::<i32>(2),
            )),
            |(sign, hr, _, min)| {
                let off = hr * 3600 + min * 60;
                if sign == Some('-') {
                    -off
                } else {
                    off
                }
            },
        ),
    ))(input)
}

impl Time {
    pub const MIN: Self = Time(i64::MIN);
    pub const MAX: Self = Time(i64::MAX);

    /// Parses a time as either 90,000ths of a second since epoch or a RFC 3339-like string.
    ///
    /// The former is 90,000ths of a second since 1970-01-01T00:00:00 UTC, excluding leap seconds.
    ///
    /// The latter is a date such as `2006-01-02T15:04:05`, followed by an optional 90,000ths of
    /// a second such as `:00001`, followed by an optional time zone offset such as `Z` or
    /// `-07:00`. A missing fraction is assumed to be 0. A missing time zone offset implies the
    /// local time zone.
    pub fn parse(input: &str) -> Result<Self, Error> {
        // First try parsing as 90,000ths of a second since epoch.
        if let Ok(i) = i64::from_str(input) {
            return Ok(Time(i));
        }

        // If that failed, parse as a time string or bust.
        let (remaining, ((tm_year, tm_mon, tm_mday), opt_time, opt_zone)) = tuple((
            parse_datepart,
            opt(preceded(tag("T"), parse_timepart)),
            opt(parse_zone),
        ))(input)
        .map_err(|e| match e {
            nom::Err::Incomplete(_) => err!(InvalidArgument, msg("incomplete")),
            nom::Err::Error(e) | nom::Err::Failure(e) => {
                err!(InvalidArgument, source(nom::error::convert_error(input, e)))
            }
        })?;
        if !remaining.is_empty() {
            bail!(
                InvalidArgument,
                msg("unexpected suffix {remaining:?} following time string")
            );
        }
        let (tm_hour, tm_min, tm_sec, subsec) = opt_time.unwrap_or((0, 0, 0, 0));
        let dt = jiff::civil::DateTime::new(tm_year, tm_mon, tm_mday, tm_hour, tm_min, tm_sec, 0)
            .map_err(|e| err!(InvalidArgument, source(e)))?;
        let tz =
            if let Some(off) = opt_zone {
                jiff::tz::TimeZone::fixed(jiff::tz::Offset::from_seconds(off).map_err(|e| {
                    err!(InvalidArgument, msg("invalid time zone offset"), source(e))
                })?)
            } else {
                global_zone()
            };
        let sec = tz
            .into_ambiguous_zoned(dt)
            .compatible()
            .map_err(|e| err!(InvalidArgument, source(e)))?
            .timestamp()
            .as_second();
        Ok(Time(sec * TIME_UNITS_PER_SEC + i64::from(subsec)))
    }

    /// Convert to unix seconds by floor method (rounding down).
    pub fn unix_seconds(&self) -> i64 {
        self.0 / TIME_UNITS_PER_SEC
    }
}

impl From<SystemTime> for Time {
    fn from(tm: SystemTime) -> Self {
        Time(tm.0.tv_sec() * TIME_UNITS_PER_SEC + tm.0.tv_nsec() * 9 / 100_000)
    }
}

impl From<jiff::Timestamp> for Time {
    fn from(tm: jiff::Timestamp) -> Self {
        Time((tm.as_nanosecond() * 9 / 100_000) as i64)
    }
}

impl std::str::FromStr for Time {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl ops::Sub for Time {
    type Output = Duration;
    fn sub(self, rhs: Time) -> Duration {
        Duration(self.0 - rhs.0)
    }
}

impl ops::AddAssign<Duration> for Time {
    fn add_assign(&mut self, rhs: Duration) {
        self.0 += rhs.0
    }
}

impl ops::Add<Duration> for Time {
    type Output = Time;
    fn add(self, rhs: Duration) -> Time {
        Time(self.0 + rhs.0)
    }
}

impl ops::Sub<Duration> for Time {
    type Output = Time;
    fn sub(self, rhs: Duration) -> Time {
        Time(self.0 - rhs.0)
    }
}

impl fmt::Debug for Time {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // Write both the raw and display forms.
        write!(f, "{} /* {} */", self.0, self)
    }
}

impl fmt::Display for Time {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let tm = jiff::Zoned::new(
            jiff::Timestamp::from_second(self.0 / TIME_UNITS_PER_SEC).map_err(|_| fmt::Error)?,
            global_zone(),
        );
        write!(
            f,
            "{}:{:05}{}",
            tm.strftime("%FT%T"),
            self.0 % TIME_UNITS_PER_SEC,
            tm.strftime("%:z"),
        )
    }
}

/// A duration specified in 1/90,000ths of a second.
/// Durations are typically non-negative, but a `moonfire_db::db::StreamDayValue::duration` may be
/// negative when used as a `<StreamDayValue as Value>::Change`.
#[derive(Clone, Copy, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Duration(pub i64);

impl From<Duration> for jiff::SignedDuration {
    fn from(d: Duration) -> Self {
        jiff::SignedDuration::from_nanos(d.0 * 100_000 / 9)
    }
}

impl TryFrom<Duration> for std::time::Duration {
    type Error = std::num::TryFromIntError;

    fn try_from(value: Duration) -> Result<Self, Self::Error> {
        Ok(std::time::Duration::from_nanos(
            u64::try_from(value.0)? * 100_000 / 9,
        ))
    }
}

impl fmt::Debug for Duration {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // Write both the raw and display forms.
        write!(f, "{} /* {} */", self.0, self)
    }
}

impl fmt::Display for Duration {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut seconds = self.0 / TIME_UNITS_PER_SEC;
        const MINUTE_IN_SECONDS: i64 = 60;
        const HOUR_IN_SECONDS: i64 = 60 * MINUTE_IN_SECONDS;
        const DAY_IN_SECONDS: i64 = 24 * HOUR_IN_SECONDS;
        let days = seconds / DAY_IN_SECONDS;
        seconds %= DAY_IN_SECONDS;
        let hours = seconds / HOUR_IN_SECONDS;
        seconds %= HOUR_IN_SECONDS;
        let minutes = seconds / MINUTE_IN_SECONDS;
        seconds %= MINUTE_IN_SECONDS;
        let mut have_written = if days > 0 {
            write!(f, "{} day{}", days, if days == 1 { "" } else { "s" })?;
            true
        } else {
            false
        };
        if hours > 0 {
            write!(
                f,
                "{}{} hour{}",
                if have_written { " " } else { "" },
                hours,
                if hours == 1 { "" } else { "s" }
            )?;
            have_written = true;
        }
        if minutes > 0 {
            write!(
                f,
                "{}{} minute{}",
                if have_written { " " } else { "" },
                minutes,
                if minutes == 1 { "" } else { "s" }
            )?;
            have_written = true;
        }
        if seconds > 0 || !have_written {
            write!(
                f,
                "{}{} second{}",
                if have_written { " " } else { "" },
                seconds,
                if seconds == 1 { "" } else { "s" }
            )?;
        }
        Ok(())
    }
}

impl std::convert::TryFrom<std::time::Duration> for Duration {
    type Error = std::num::TryFromIntError;

    fn try_from(value: std::time::Duration) -> Result<Self, Self::Error> {
        Ok(Self(i64::try_from(value.as_nanos() * 9 / 100_000)?))
    }
}

impl ops::Mul<i64> for Duration {
    type Output = Self;
    fn mul(self, rhs: i64) -> Self::Output {
        Duration(self.0 * rhs)
    }
}

impl std::ops::Neg for Duration {
    type Output = Self;
    fn neg(self) -> Self::Output {
        Duration(-self.0)
    }
}

impl ops::Add for Duration {
    type Output = Duration;
    fn add(self, rhs: Duration) -> Duration {
        Duration(self.0 + rhs.0)
    }
}

impl ops::AddAssign for Duration {
    fn add_assign(&mut self, rhs: Duration) {
        self.0 += rhs.0
    }
}

impl ops::SubAssign for Duration {
    fn sub_assign(&mut self, rhs: Duration) {
        self.0 -= rhs.0
    }
}

pub mod testutil {
    pub fn init_zone() {
        super::init_zone(|| {
            jiff::tz::TimeZone::get("America/Los_Angeles")
                .expect("America/Los_Angeles should exist")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Duration, Time, TIME_UNITS_PER_SEC};
    use std::convert::TryFrom;

    #[test]
    fn test_parse_time() {
        super::testutil::init_zone();
        #[rustfmt::skip]
        let tests = &[
            ("2006-01-02T15:04:05-07:00",       102261550050000),
            ("2006-01-02T15:04:05:00001-07:00", 102261550050001),
            ("2006-01-02T15:04:05-08:00",       102261874050000),
            ("2006-01-02T15:04:05",             102261874050000), // implied -08:00
            ("2006-01-02T15:04",                102261873600000), // implied -08:00
            ("2006-01-02T15:04:05:00001",       102261874050001), // implied -08:00
            ("2006-01-02T15:04:05-00:00",       102259282050000),
            ("2006-01-02T15:04:05Z",            102259282050000),
            ("2006-01-02-08:00",                102256992000000), // implied -08:00
            ("2006-01-02",                      102256992000000), // implied -08:00
            ("2006-01-02Z",                     102254400000000),
            ("102261550050000",                 102261550050000),
        ];
        for test in tests {
            assert_eq!(test.1, Time::parse(test.0).unwrap().0, "parsing {}", test.0);
        }
    }

    #[test]
    fn test_format_time() {
        super::testutil::init_zone();
        assert_eq!(
            "2006-01-02T15:04:05:00000-08:00",
            format!("{}", Time(102261874050000))
        );
    }

    #[test]
    fn test_display_duration() {
        let tests = &[
            // (output, seconds)
            ("0 seconds", 0),
            ("1 second", 1),
            ("1 minute", 60),
            ("1 minute 1 second", 61),
            ("2 minutes", 120),
            ("1 hour", 3600),
            ("1 hour 1 minute", 3660),
            ("2 hours", 7200),
            ("1 day", 86400),
            ("1 day 1 hour", 86400 + 3600),
            ("2 days", 2 * 86400),
        ];
        for test in tests {
            assert_eq!(test.0, format!("{}", Duration(test.1 * TIME_UNITS_PER_SEC)));
        }
    }

    #[test]
    fn test_duration_from_std_duration() {
        assert_eq!(
            Duration::try_from(std::time::Duration::new(1, 11111)),
            Ok(Duration(90_000))
        );
        assert_eq!(
            Duration::try_from(std::time::Duration::new(1, 11112)),
            Ok(Duration(90_001))
        );
        assert_eq!(
            Duration::try_from(std::time::Duration::new(60, 0)),
            Ok(Duration(60 * TIME_UNITS_PER_SEC))
        );
        Duration::try_from(std::time::Duration::new(u64::MAX, 0)).unwrap_err();
    }
}
