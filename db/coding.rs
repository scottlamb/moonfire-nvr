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

//! Binary encoding/decoding.

/// Zigzag-encodes a signed integer, as in [protocol buffer
/// encoding](https://developers.google.com/protocol-buffers/docs/encoding#types). Uses the low bit
/// to indicate signedness (1 = negative, 0 = non-negative).
#[inline(always)]
pub fn zigzag32(i: i32) -> u32 { ((i << 1) as u32) ^ ((i >> 31) as u32) }

/// Zigzag-decodes to a signed integer.
/// See `zigzag`.
#[inline(always)]
pub fn unzigzag32(i: u32) -> i32 { ((i >> 1) as i32) ^ -((i & 1) as i32) }

#[inline(always)]
pub fn decode_varint32(data: &[u8], i: usize) -> Result<(u32, usize), ()> {
    // Unroll a few likely possibilities before going into the robust out-of-line loop.
    // This aids branch prediction.
    if data.len() > i && (data[i] & 0x80) == 0 {
        return Ok((data[i] as u32, i+1))
    } else if data.len() > i + 1 && (data[i+1] & 0x80) == 0 {
        return Ok((( (data[i] & 0x7f) as u32)          |
                   (( data[i+1]       as u32) << 7),
                   i+2))
    } else if data.len() > i + 2 && (data[i+2] & 0x80) == 0 {
        return Ok((( (data[i]   & 0x7f) as u32)        |
                   (((data[i+1] & 0x7f) as u32) <<  7) |
                   (( data[i+2]         as u32) << 14),
                   i+3))
    }
    decode_varint32_slow(data, i)
}

#[cold]
fn decode_varint32_slow(data: &[u8], mut i: usize) -> Result<(u32, usize), ()> {
    let l = data.len();
    let mut out = 0;
    let mut shift = 0;
    loop {
        if i == l {
            return Err(())
        }
        let b = data[i];
        if shift == 28 && (b & 0xf0) != 0 {
            return Err(())
        }
        out |= ((b & 0x7f) as u32) << shift;
        shift += 7;
        i += 1;
        if (b & 0x80) == 0 {
            break;
        }
    }
    Ok((out, i))
}

pub fn append_varint32(i: u32, data: &mut Vec<u8>) {
    if i < 1u32 << 7 {
        data.push(i as u8);
    } else if i < 1u32 << 14 {
        data.extend_from_slice(&[(( i        & 0x7F) | 0x80) as u8,
                                   (i >>  7)                 as u8]);
    } else if i < 1u32 << 21 {
        data.extend_from_slice(&[(( i        & 0x7F) | 0x80) as u8,
                                 (((i >>  7) & 0x7F) | 0x80) as u8,
                                   (i >> 14)                 as u8]);
    } else if i < 1u32 << 28 {
        data.extend_from_slice(&[(( i        & 0x7F) | 0x80) as u8,
                                 (((i >>  7) & 0x7F) | 0x80) as u8,
                                 (((i >> 14) & 0x7F) | 0x80) as u8,
                                   (i >> 21)                 as u8]);
    } else {
        data.extend_from_slice(&[(( i        & 0x7F) | 0x80) as u8,
                                 (((i >>  7) & 0x7F) | 0x80) as u8,
                                 (((i >> 14) & 0x7F) | 0x80) as u8,
                                 (((i >> 21) & 0x7F) | 0x80) as u8,
                                   (i >> 28)                 as u8]);
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zigzag() {
        struct Test {
            decoded: i32,
            encoded: u32,
        }
        let tests = [
            Test{decoded: 0,           encoded: 0},
            Test{decoded: -1,          encoded: 1},
            Test{decoded: 1,           encoded: 2},
            Test{decoded: -2,          encoded: 3},
            Test{decoded: 2147483647,  encoded: 4294967294},
            Test{decoded: -2147483648, encoded: 4294967295},
        ];
        for test in &tests {
            assert_eq!(test.encoded, zigzag32(test.decoded));
            assert_eq!(test.decoded, unzigzag32(test.encoded));
        }
    }

    #[test]
    fn test_correct_varints() {
        struct Test {
            decoded: u32,
            encoded: &'static [u8],
        }
        let tests = [
            Test{decoded: 1,          encoded: b"\x01"},
            Test{decoded: 257,        encoded: b"\x81\x02"},
            Test{decoded: 49409,      encoded: b"\x81\x82\x03"},
            Test{decoded: 8438017,    encoded: b"\x81\x82\x83\x04"},
            Test{decoded: 1350615297, encoded: b"\x81\x82\x83\x84\x05"},
        ];
        for test in &tests {
            // Test encoding to an empty buffer.
            let mut out = Vec::new();
            append_varint32(test.decoded, &mut out);
            assert_eq!(&out[..], test.encoded);

            // ...and to a non-empty buffer.
            let mut buf = Vec::new();
            out.clear();
            out.push(b'x');
            buf.push(b'x');
            buf.extend_from_slice(test.encoded);
            append_varint32(test.decoded, &mut out);
            assert_eq!(out, buf);

            // Test decoding from the beginning of the string.
            assert_eq!((test.decoded, test.encoded.len()),
                       decode_varint32(test.encoded, 0).unwrap());

            // ...and from the middle of a buffer.
            buf.push(b'x');
            assert_eq!((test.decoded, test.encoded.len() + 1),
                       decode_varint32(&buf, 1).unwrap());
        }
    }

    #[test]
    fn test_bad_varints() {
        let tests: &[&[u8]] = &[
            // buffer underruns
            b"",
            b"\x80",
            b"\x80\x80",
            b"\x80\x80\x80",
            b"\x80\x80\x80\x80",

            // int32 overflows
            b"\x80\x80\x80\x80\x80",
            b"\x80\x80\x80\x80\x80\x00",
        ];
        for (i, encoded) in tests.iter().enumerate() {
            assert!(decode_varint32(encoded, 0).is_err(), "while on test {}", i);
        }
    }
}
