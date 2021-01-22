// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2016 The Moonfire NVR Authors
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

use nom::IResult;
use nom::branch::alt;
use nom::bytes::complete::{tag, take_while1};
use nom::character::complete::space0;
use nom::combinator::{map, map_res, opt};
use nom::sequence::{delimited, tuple};
use std::fmt::Write as _;

static MULTIPLIERS: [(char, u64); 4] = [
    // (suffix character, power of 2)
    ('T', 40),
    ('G', 30),
    ('M', 20),
    ('K', 10),
];

/// Encodes a non-negative size into human-readable form.
pub fn encode_size(mut raw: i64) -> String {
    let mut encoded = String::new();
    for &(c, n) in &MULTIPLIERS {
        if raw >= 1i64<<n {
            write!(&mut encoded, "{}{} ", raw >> n, c).unwrap();
            raw &= (1i64 << n) - 1;
        }
    }
    if raw > 0 || encoded.len() == 0 {
        write!(&mut encoded, "{}", raw).unwrap();
    } else {
        encoded.pop();  // remove trailing space.
    }
    encoded
}

fn decode_sizepart(input: &str) -> IResult<&str, i64> {
    map(
        tuple((
            map_res(take_while1(|c: char| c.is_ascii_digit()),
                    |input: &str| i64::from_str_radix(input, 10)),
            opt(alt((
                nom::combinator::value(1<<40, tag("T")),
                nom::combinator::value(1<<30, tag("G")),
                nom::combinator::value(1<<20, tag("M")),
                nom::combinator::value(1<<10, tag("K"))
            )))
        )),
        |(n, opt_unit)| n * opt_unit.unwrap_or(1)
    )(input)
}

fn decode_size_internal(input: &str) -> IResult<&str, i64> {
    nom::multi::fold_many1(
        delimited(space0, decode_sizepart, space0),
        0,
        |sum, i| sum + i)(input)
}

/// Decodes a human-readable size as output by encode_size.
pub fn decode_size(encoded: &str) -> Result<i64, ()> {
    let (remaining, decoded) = decode_size_internal(encoded).map_err(|_e| ())?;
    if !remaining.is_empty() {
        return Err(());
    }
    Ok(decoded)
}

/// Returns a hex-encoded version of the input.
pub fn hex(raw: &[u8]) -> String {
    const HEX_CHARS: [u8; 16] = [b'0', b'1', b'2', b'3', b'4', b'5', b'6', b'7',
                                 b'8', b'9', b'a', b'b', b'c', b'd', b'e', b'f'];
    let mut hex = Vec::with_capacity(2 * raw.len());
    for b in raw {
        hex.push(HEX_CHARS[((b & 0xf0) >> 4) as usize]);
        hex.push(HEX_CHARS[( b & 0x0f      ) as usize]);
    }
    unsafe { String::from_utf8_unchecked(hex) }
}

/// Returns [0, 16) or error.
fn dehex_byte(hex_byte: u8) -> Result<u8, ()> {
    match hex_byte {
        b'0' ..= b'9' => Ok(hex_byte - b'0'),
        b'a' ..= b'f' => Ok(hex_byte - b'a' + 10),
        _ => Err(()),
    }
}

/// Returns a 20-byte raw form of the given hex string.
/// (This is the size of a SHA1 hash, the only current use of this function.)
pub fn dehex(hexed: &[u8]) -> Result<[u8; 20], ()> {
    if hexed.len() != 40 {
        return Err(());
    }
    let mut out = [0; 20];
    for i in 0..20 {
        out[i] = (dehex_byte(hexed[i<<1])? << 4) + dehex_byte(hexed[(i<<1) + 1])?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode() {
        assert_eq!(super::decode_size("100M").unwrap(), 100i64 << 20);
        assert_eq!(super::decode_size("100M 42").unwrap(), (100i64 << 20) + 42);
    }

    #[test]
    fn round_trip() {
        let s = "de382684a471f178e4e3a163762711b0653bfd83";
        let dehexed = dehex(s.as_bytes()).unwrap();
        assert_eq!(&hex(&dehexed[..]), s);
    }

    #[test]
    fn dehex_errors() {
        dehex(b"").unwrap_err();
        dehex(b"de382684a471f178e4e3a163762711b0653bfd8g").unwrap_err();
    }
}
