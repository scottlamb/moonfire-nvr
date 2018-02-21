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

//! H.264 decoding
//!
//! For the most part, Moonfire NVR does not try to understand the video codec. However, H.264 has
//! two byte stream encodings: ISO/IEC 14496-10 Annex B, and ISO/IEC 14496-15 AVC access units.
//! When streaming from RTSP, ffmpeg supplies the former. We need the latter to stick into `.mp4`
//! files. This file manages the conversion, both for the ffmpeg "extra data" (which should become
//! the ISO/IEC 14496-15 section 5.2.4.1 `AVCDecoderConfigurationRecord`) and the actual samples.
//!
//! ffmpeg of course has logic to do the same thing, but unfortunately it is not exposed except
//! through ffmpeg's own generated `.mp4` file. Extracting just this part of their `.mp4` files
//! would be more trouble than it's worth.

use byteorder::{BigEndian, WriteBytesExt};
use failure::Error;
use regex::bytes::Regex;

// See ISO/IEC 14496-10 table 7-1 - NAL unit type codes, syntax element categories, and NAL unit
// type classes.
const NAL_UNIT_SEQ_PARAMETER_SET: u8 = 7;
const NAL_UNIT_PIC_PARAMETER_SET: u8 = 8;

const NAL_UNIT_TYPE_MASK: u8 = 0x1F;  // bottom 5 bits of first byte of unit.

/// Decodes a H.264 Annex B byte stream into NAL units. Calls `f` for each NAL unit in the byte
/// stream. Aborts if `f` returns error.
///
/// See ISO/IEC 14496-10 section B.2: Byte stream NAL unit decoding process.
/// This is a relatively simple, unoptimized implementation.
///
/// TODO: detect invalid byte streams. For example, several 0x00s not followed by a 0x01, a stream
/// stream not starting with 0x00 0x00 0x00 0x01, or an empty NAL unit.
fn decode_h264_annex_b<'a, F>(data: &'a [u8], mut f: F) -> Result<(), Error>
where F: FnMut(&'a [u8]) -> Result<(), Error> {
    lazy_static! {
        static ref START_CODE: Regex = Regex::new(r"(\x00{2,}\x01)").unwrap();
    }
    for unit in START_CODE.split(data) {
        if !unit.is_empty() {
            f(unit)?;
        }
    }
    Ok(())
}

/// Parses Annex B extra data, returning a tuple holding the `sps` and `pps` substrings.
fn parse_annex_b_extra_data(data: &[u8]) -> Result<(&[u8], &[u8]), Error> {
    let mut sps = None;
    let mut pps = None;
    decode_h264_annex_b(data, |unit| {
        let nal_type = (unit[0] as u8) & NAL_UNIT_TYPE_MASK;
        match nal_type {
            NAL_UNIT_SEQ_PARAMETER_SET => sps = Some(unit),
            NAL_UNIT_PIC_PARAMETER_SET => pps = Some(unit),
            _ => bail!("Expected SPS and PPS; got type {}", nal_type),
        };
        Ok(())
    })?;
    match (sps, pps) {
        (Some(s), Some(p)) => Ok((s, p)),
        _ => bail!("SPS and PPS must be specified"),
    }
}

/// Parsed representation of ffmpeg's "extradata".
#[derive(Debug, PartialEq, Eq)]
pub struct ExtraData {
    pub sample_entry: Vec<u8>,
    pub rfc6381_codec: String,
    pub width: u16,
    pub height: u16,

    /// True iff sample data should be transformed from Annex B format to AVC format via a call to
    /// `transform_sample_data`. (The assumption is that if the extra data was in Annex B format,
    /// the sample data is also.)
    pub need_transform: bool,
}

impl ExtraData {
    /// Parses "extradata" from ffmpeg. This data may be in either Annex B format or AVC format.
    pub fn parse(extradata: &[u8], width: u16, height: u16) -> Result<ExtraData, Error> {
        let mut sps_and_pps = None;
        let need_transform;
        let avcc_len = if extradata.starts_with(b"\x00\x00\x00\x01") ||
                          extradata.starts_with(b"\x00\x00\x01") {
            // ffmpeg supplied "extradata" in Annex B format.
            let (s, p) = parse_annex_b_extra_data(extradata)?;
            sps_and_pps = Some((s, p));
            need_transform = true;

            // This magic value is checked at the end of the function;
            // unit tests confirm its accuracy.
            19 + s.len() + p.len()
        } else {
            // Assume "extradata" holds an AVCDecoderConfiguration.
            need_transform = false;
            8 + extradata.len()
        };
        let sps_and_pps = sps_and_pps;
        let need_transform = need_transform;

        // This magic value is also checked at the end.
        let avc1_len = 86 + avcc_len;

        let mut sample_entry = Vec::with_capacity(avc1_len);

        // This is a concatenation of the following boxes/classes.

        // SampleEntry, ISO/IEC 14496-12 section 8.5.2.
        let avc1_len_pos = sample_entry.len();
        sample_entry.write_u32::<BigEndian>(avc1_len as u32)?;  // length
        // type + reserved + data_reference_index = 1
        sample_entry.extend_from_slice(b"avc1\x00\x00\x00\x00\x00\x00\x00\x01");

        // VisualSampleEntry, ISO/IEC 14496-12 section 12.1.3.
        sample_entry.extend_from_slice(&[0; 16]);  // pre-defined + reserved
        sample_entry.write_u16::<BigEndian>(width)?;
        sample_entry.write_u16::<BigEndian>(height)?;
        sample_entry.extend_from_slice(&[
                0x00, 0x48, 0x00, 0x00,  // horizresolution
                0x00, 0x48, 0x00, 0x00,  // vertresolution
                0x00, 0x00, 0x00, 0x00,  // reserved
                0x00, 0x01,              // frame count
                0x00, 0x00, 0x00, 0x00,  // compressorname
                0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00,
                0x00, 0x18, 0xff, 0xff,  // depth + pre_defined
        ]);

        // AVCSampleEntry, ISO/IEC 14496-15 section 5.3.4.1.
        // AVCConfigurationBox, ISO/IEC 14496-15 section 5.3.4.1.
        let avcc_len_pos = sample_entry.len();
        sample_entry.write_u32::<BigEndian>(avcc_len as u32)?;  // length
        sample_entry.extend_from_slice(b"avcC");

        let avc_decoder_config_len = if let Some((sps, pps)) = sps_and_pps {
            let before = sample_entry.len();

            // Create the AVCDecoderConfiguration, ISO/IEC 14496-15 section 5.2.4.1.
            // The beginning of the AVCDecoderConfiguration takes a few values from
            // the SPS (ISO/IEC 14496-10 section 7.3.2.1.1). One caveat: that section
            // defines the syntax in terms of RBSP, not NAL. The difference is the
            // escaping of 00 00 01 and 00 00 02; see notes about
            // "emulation_prevention_three_byte" in ISO/IEC 14496-10 section 7.4.
            // It looks like 00 is not a valid value of profile_idc, so this distinction
            // shouldn't be relevant here. And ffmpeg seems to ignore it.
            sample_entry.push(1);       // configurationVersion
            sample_entry.push(sps[1]);  // profile_idc . AVCProfileIndication
            sample_entry.push(sps[2]);  // ...misc bits... . profile_compatibility
            sample_entry.push(sps[3]);  // level_idc . AVCLevelIndication

            // Hardcode lengthSizeMinusOne to 3, matching TransformSampleData's 4-byte
            // lengths.
            sample_entry.push(0xff);

            // Only support one SPS and PPS.
            // ffmpeg's ff_isom_write_avcc has the same limitation, so it's probably
            // fine. This next byte is a reserved 0b111 + a 5-bit # of SPSs (1).
            sample_entry.push(0xe1);
            sample_entry.write_u16::<BigEndian>(sps.len() as u16)?;
            sample_entry.extend_from_slice(sps);
            sample_entry.push(1);  // # of PPSs.
            sample_entry.write_u16::<BigEndian>(pps.len() as u16)?;
            sample_entry.extend_from_slice(pps);

            if sample_entry.len() - avcc_len_pos != avcc_len {
                bail!("internal error: anticipated AVCConfigurationBox \
                       length {}, but was actually {}; sps length {}, pps length {}",
                      avcc_len, sample_entry.len() - avcc_len_pos, sps.len(), pps.len());
            }
            sample_entry.len() - before
        } else {
            sample_entry.extend_from_slice(extradata);
            extradata.len()
        };

        if sample_entry.len() - avc1_len_pos != avc1_len {
            bail!("internal error: anticipated AVCSampleEntry length \
                   {}, but was actually {}; AVCDecoderConfiguration length {}",
                  avc1_len, sample_entry.len() - avc1_len_pos, avc_decoder_config_len);
        }
        let profile_idc = sample_entry[103];
        let constraint_flags = sample_entry[104];
        let level_idc = sample_entry[105];
        let codec = format!("avc1.{:02x}{:02x}{:02x}", profile_idc, constraint_flags, level_idc);
        Ok(ExtraData {
            sample_entry,
            rfc6381_codec: codec,
            width,
            height,
            need_transform,
        })
    }
}

/// Transforms sample data from Annex B format to AVC format. Should be called on samples iff
/// `ExtraData::need_transform` is true. Uses an out parameter `avc_sample` rather than a return
/// so that memory allocations can be reused from sample to sample.
pub fn transform_sample_data(annexb_sample: &[u8], avc_sample: &mut Vec<u8>) -> Result<(), Error> {
    // See AVCParameterSamples, ISO/IEC 14496-15 section 5.3.2.
    avc_sample.clear();

    // The output will be about as long as the input. Annex B stop codes require at least three
    // bytes; many seem to be four. The output lengths are exactly four.
    avc_sample.reserve(annexb_sample.len() + 4);
    decode_h264_annex_b(annexb_sample, |unit| {
        // 4-byte length; this must match ParseExtraData's lengthSizeMinusOne == 3.
        avc_sample.write_u32::<BigEndian>(unit.len() as u32)?;  // length
        avc_sample.extend_from_slice(unit);
        Ok(())
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use db::testutil;

    const ANNEX_B_TEST_INPUT: [u8; 35] = [
        0x00, 0x00, 0x00, 0x01, 0x67, 0x4d, 0x00, 0x1f,
        0x9a, 0x66, 0x02, 0x80, 0x2d, 0xff, 0x35, 0x01,
        0x01, 0x01, 0x40, 0x00, 0x00, 0xfa, 0x00, 0x00,
        0x1d, 0x4c, 0x01, 0x00, 0x00, 0x00, 0x01, 0x68,
        0xee, 0x3c, 0x80,
    ];

    const AVC_DECODER_CONFIG_TEST_INPUT: [u8; 38] = [
        0x01, 0x4d, 0x00, 0x1f, 0xff, 0xe1, 0x00, 0x17,
        0x67, 0x4d, 0x00, 0x1f, 0x9a, 0x66, 0x02, 0x80,
        0x2d, 0xff, 0x35, 0x01, 0x01, 0x01, 0x40, 0x00,
        0x00, 0xfa, 0x00, 0x00, 0x1d, 0x4c, 0x01, 0x01,
        0x00, 0x04, 0x68, 0xee, 0x3c, 0x80,
    ];

    const TEST_OUTPUT: [u8; 132] = [
        0x00, 0x00, 0x00, 0x84, 0x61, 0x76, 0x63, 0x31,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x05, 0x00, 0x02, 0xd0, 0x00, 0x48, 0x00, 0x00,
        0x00, 0x48, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x18, 0xff, 0xff, 0x00, 0x00,
        0x00, 0x2e, 0x61, 0x76, 0x63, 0x43, 0x01, 0x4d,
        0x00, 0x1f, 0xff, 0xe1, 0x00, 0x17, 0x67, 0x4d,
        0x00, 0x1f, 0x9a, 0x66, 0x02, 0x80, 0x2d, 0xff,
        0x35, 0x01, 0x01, 0x01, 0x40, 0x00, 0x00, 0xfa,
        0x00, 0x00, 0x1d, 0x4c, 0x01, 0x01, 0x00, 0x04,
        0x68, 0xee, 0x3c, 0x80,
    ];

    #[test]
    fn test_decode() {
        testutil::init();
        let data = &ANNEX_B_TEST_INPUT;
        let mut pieces = Vec::new();
        super::decode_h264_annex_b(data, |p| {
            pieces.push(p);
            Ok(())
        }).unwrap();
        assert_eq!(&pieces, &[&data[4 .. 27], &data[31 ..]]);
    }

    #[test]
    fn test_sample_entry_from_avc_decoder_config() {
        testutil::init();
        let e = super::ExtraData::parse(&AVC_DECODER_CONFIG_TEST_INPUT, 1280, 720).unwrap();
        assert_eq!(&e.sample_entry[..], &TEST_OUTPUT[..]);
        assert_eq!(e.width, 1280);
        assert_eq!(e.height, 720);
        assert_eq!(e.need_transform, false);
        assert_eq!(e.rfc6381_codec, "avc1.4d001f");
    }

    #[test]
    fn test_sample_entry_from_annex_b() {
        testutil::init();
        let e = super::ExtraData::parse(&ANNEX_B_TEST_INPUT, 1280, 720).unwrap();
        assert_eq!(e.width, 1280);
        assert_eq!(e.height, 720);
        assert_eq!(e.need_transform, true);
        assert_eq!(e.rfc6381_codec, "avc1.4d001f");
    }

    #[test]
    fn test_transform_sample_data() {
        testutil::init();
        const INPUT: [u8; 64] = [
            0x00, 0x00, 0x00, 0x01, 0x67, 0x4d, 0x00, 0x1f,
            0x9a, 0x66, 0x02, 0x80, 0x2d, 0xff, 0x35, 0x01,
            0x01, 0x01, 0x40, 0x00, 0x00, 0xfa, 0x00, 0x00,
            0x1d, 0x4c, 0x01,

            0x00, 0x00, 0x00, 0x01, 0x68, 0xee, 0x3c, 0x80,

            0x00, 0x00, 0x00, 0x01, 0x06, 0x06, 0x01, 0xc4,
            0x80,

            0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x80, 0x10,
            0x00, 0x08, 0x7f, 0x00, 0x5d, 0x27, 0xb5, 0xc1,
            0xff, 0x8c, 0xd6, 0x35,
            // (truncated)
        ];
        const EXPECTED_OUTPUT: [u8; 64] = [
            0x00, 0x00, 0x00, 0x17, 0x67, 0x4d, 0x00, 0x1f,
            0x9a, 0x66, 0x02, 0x80, 0x2d, 0xff, 0x35, 0x01,
            0x01, 0x01, 0x40, 0x00, 0x00, 0xfa, 0x00, 0x00,
            0x1d, 0x4c, 0x01,

            0x00, 0x00, 0x00, 0x04, 0x68, 0xee, 0x3c, 0x80,

            0x00, 0x00, 0x00, 0x05, 0x06, 0x06, 0x01, 0xc4,
            0x80,

            0x00, 0x00, 0x00, 0x10, 0x65, 0x88, 0x80, 0x10,
            0x00, 0x08, 0x7f, 0x00, 0x5d, 0x27, 0xb5, 0xc1,
            0xff, 0x8c, 0xd6, 0x35,
        ];
        let mut out = Vec::new();
        super::transform_sample_data(&INPUT, &mut out).unwrap();
        assert_eq!(&out[..], &EXPECTED_OUTPUT[..]);
    }
}
