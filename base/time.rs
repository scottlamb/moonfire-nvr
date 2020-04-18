// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2016-2020 The Moonfire NVR Authors
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

//! Time and durations for Moonfire NVR's internal format.

use failure::{Error, bail, format_err};
use nom::branch::alt;
use nom::bytes::complete::{tag, take_while_m_n};
use nom::combinator::{map, map_res, opt};
use nom::sequence::{preceded, tuple};
use std::ops;
use std::fmt;
use std::str::FromStr;
use time;

type IResult<'a, I, O> = nom::IResult<I, O, nom::error::VerboseError<&'a str>>;

pub const TIME_UNITS_PER_SEC: i64 = 90_000;

/// A time specified as 90,000ths of a second since 1970-01-01 00:00:00 UTC.
#[derive(Clone, Copy, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct Time(pub i64);

/// Returns a parser for a `len`-digit non-negative number which fits into an i32.
fn fixed_len_num<'a>(len: usize) -> impl Fn(&'a str) -> IResult<&'a str, i32> {
    map_res(take_while_m_n(len, len, |c: char| c.is_ascii_digit()),
            |input: &str| i32::from_str_radix(input, 10))
}

/// Parses `YYYY-mm-dd` into pieces.
fn parse_datepart(input: &str) -> IResult<&str, (i32, i32, i32)> {
    tuple((
        fixed_len_num(4),
        preceded(tag("-"), fixed_len_num(2)),
        preceded(tag("-"), fixed_len_num(2))
    ))(input)
}

/// Parses `HH:MM[:SS[:FFFFF]]` into pieces.
fn parse_timepart(input: &str) -> IResult<&str, (i32, i32, i32, i32)> {
    let (input, (hr, _, min)) = tuple((fixed_len_num(2), tag(":"), fixed_len_num(2)))(input)?;
    let (input, stuff) = opt(tuple((
                preceded(tag(":"), fixed_len_num(2)),
                opt(preceded(tag(":"), fixed_len_num(5)))
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
                        fixed_len_num(2),
                        tag(":"),
                        fixed_len_num(2)
                )),
                |(sign, hr, _, min)| {
                    let off = hr * 3600 + min * 60;
                    if sign == Some('-') { off } else { -off }
                })
    ))(input)
}

impl Time {
    pub fn new(tm: time::Timespec) -> Self {
        Time(tm.sec * TIME_UNITS_PER_SEC + tm.nsec as i64 * TIME_UNITS_PER_SEC / 1_000_000_000)
    }

    pub const fn min_value() -> Self { Time(i64::min_value()) }
    pub const fn max_value() -> Self { Time(i64::max_value()) }

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
        match i64::from_str(input) {
            Ok(i) => return Ok(Time(i)),
            Err(_) => {},
        }

        // If that failed, parse as a time string or bust.
        let (remaining, ((tm_year, tm_mon, tm_mday), opt_time, opt_zone)) =
            tuple((parse_datepart,
                   opt(preceded(tag("T"), parse_timepart)),
                   opt(parse_zone)))(input)
            .map_err(|e| match e {
                nom::Err::Incomplete(_) => format_err!("incomplete"),
                nom::Err::Error(e) | nom::Err::Failure(e) => {
                    format_err!("{}", nom::error::convert_error(input, e))
                }
            })?;
        if remaining != "" {
            bail!("unexpected suffix {:?} following time string", remaining);
        }
        let (tm_hour, tm_min, tm_sec, subsec) = opt_time.unwrap_or((0, 0, 0, 0));
        let mut tm = time::Tm {
            tm_sec,
            tm_min,
            tm_hour,
            tm_mday,
            tm_mon,
            tm_year,
            tm_wday: 0,
            tm_yday: 0,
            tm_isdst: -1,
            tm_utcoff: 0,
            tm_nsec: 0,
        };
        if tm.tm_mon == 0 {
            bail!("time {:?} has month 0", input);
        }
        tm.tm_mon -= 1;
        if tm.tm_year < 1900 {
            bail!("time {:?} has year before 1900", input);
        }
        tm.tm_year -= 1900;

        // The time crate doesn't use tm_utcoff properly; it just calls timegm() if tm_utcoff == 0,
        // mktime() otherwise. If a zone is specified, use the timegm path and a manual offset.
        // If no zone is specified, use the tm_utcoff path. This is pretty lame, but follow the
        // chrono crate's lead and just use 0 or 1 to choose between these functions.
        let sec = if let Some(off) = opt_zone {
            tm.to_timespec().sec + i64::from(off)
        } else {
            tm.tm_utcoff = 1;
            tm.to_timespec().sec
        };
        Ok(Time(sec * TIME_UNITS_PER_SEC + i64::from(subsec)))
    }

    /// Convert to unix seconds by floor method (rounding down).
    pub fn unix_seconds(&self) -> i64 { self.0 / TIME_UNITS_PER_SEC }
}

impl std::str::FromStr for Time {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> { Self::parse(s) }
}

impl ops::Sub for Time {
    type Output = Duration;
    fn sub(self, rhs: Time) -> Duration { Duration(self.0 - rhs.0) }
}

impl ops::AddAssign<Duration> for Time {
    fn add_assign(&mut self, rhs: Duration) { self.0 += rhs.0 }
}

impl ops::Add<Duration> for Time {
    type Output = Time;
    fn add(self, rhs: Duration) -> Time { Time(self.0 + rhs.0) }
}

impl ops::Sub<Duration> for Time {
    type Output = Time;
    fn sub(self, rhs: Duration) -> Time { Time(self.0 - rhs.0) }
}

impl fmt::Debug for Time {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // Write both the raw and display forms.
        write!(f, "{} /* {} */", self.0, self)
    }
}

impl fmt::Display for Time {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let tm = time::at(time::Timespec{sec: self.0 / TIME_UNITS_PER_SEC, nsec: 0});
        let zone_minutes = tm.tm_utcoff.abs() / 60;
        write!(f, "{}:{:05}{}{:02}:{:02}", tm.strftime("%FT%T").or_else(|_| Err(fmt::Error))?,
               self.0 % TIME_UNITS_PER_SEC,
               if tm.tm_utcoff > 0 { '+' } else { '-' }, zone_minutes / 60, zone_minutes % 60)
    }
}

/// A duration specified in 1/90,000ths of a second.
/// Durations are typically non-negative, but a `moonfire_db::db::CameraDayValue::duration` may be
/// negative.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct Duration(pub i64);

impl Duration {
    pub fn to_tm_duration(&self) -> time::Duration {
        time::Duration::nanoseconds(self.0 * 100000 / 9)
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
            write!(f, "{}{} hour{}", if have_written { " " } else { "" },
                   hours, if hours == 1 { "" } else { "s" })?;
            have_written = true;
        }
        if minutes > 0 {
            write!(f, "{}{} minute{}", if have_written { " " } else { "" },
                   minutes, if minutes == 1 { "" } else { "s" })?;
            have_written = true;
        }
        if seconds > 0 || !have_written {
            write!(f, "{}{} second{}", if have_written { " " } else { "" },
                   seconds, if seconds == 1 { "" } else { "s" })?;
        }
        Ok(())
    }
}

impl ops::Add for Duration {
    type Output = Duration;
    fn add(self, rhs: Duration) -> Duration { Duration(self.0 + rhs.0) }
}

impl ops::AddAssign for Duration {
    fn add_assign(&mut self, rhs: Duration) { self.0 += rhs.0 }
}

impl ops::SubAssign for Duration {
    fn sub_assign(&mut self, rhs: Duration) { self.0 -= rhs.0 }
}

#[cfg(test)]
mod tests {
    use super::{Duration, Time, TIME_UNITS_PER_SEC};

    #[test]
    fn test_parse_time() {
        std::env::set_var("TZ", "America/Los_Angeles");
        time::tzset();
        let tests = &[
            ("2006-01-02T15:04:05-07:00",       102261550050000),
            ("2006-01-02T15:04:05:00001-07:00", 102261550050001),
            ("2006-01-02T15:04:05-08:00",       102261874050000),
            ("2006-01-02T15:04:05",             102261874050000),  // implied -08:00
            ("2006-01-02T15:04",                102261873600000),  // implied -08:00
            ("2006-01-02T15:04:05:00001",       102261874050001),  // implied -08:00
            ("2006-01-02T15:04:05-00:00",       102259282050000),
            ("2006-01-02T15:04:05Z",            102259282050000),
            ("2006-01-02-08:00",                102256992000000),  // implied -08:00
            ("2006-01-02",                      102256992000000),  // implied -08:00
            ("2006-01-02Z",                     102254400000000),
            ("102261550050000",                 102261550050000),
        ];
        for test in tests {
            assert_eq!(test.1, Time::parse(test.0).unwrap().0, "parsing {}", test.0);
        }
    }

    #[test]
    fn test_format_time() {
        std::env::set_var("TZ", "America/Los_Angeles");
        time::tzset();
        assert_eq!("2006-01-02T15:04:05:00000-08:00", format!("{}", Time(102261874050000)));
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
}
