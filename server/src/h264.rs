// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2020 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

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

use byteorder::{BigEndian, ByteOrder, WriteBytesExt};
use failure::{bail, format_err, Error};
use std::convert::TryFrom;

// See ISO/IEC 14496-10 table 7-1 - NAL unit type codes, syntax element categories, and NAL unit
// type classes.
const NAL_UNIT_SEQ_PARAMETER_SET: u8 = 7;
const NAL_UNIT_PIC_PARAMETER_SET: u8 = 8;

const NAL_UNIT_TYPE_MASK: u8 = 0x1F; // bottom 5 bits of first byte of unit.

// For certain common sub stream anamorphic resolutions, add a pixel aspect ratio box.
const PIXEL_ASPECT_RATIOS: [((u16, u16), (u16, u16)); 4] = [
    ((320, 240), (4, 3)),
    ((352, 240), (40, 33)),
    ((640, 480), (4, 3)),
    ((704, 480), (40, 33)),
];

/// Get the pixel aspect ratio to use if none is specified.
///
/// The Dahua IPC-HDW5231R-Z sets the aspect ratio in the H.264 SPS (correctly) for both square and
/// non-square pixels. The Hikvision DS-2CD2032-I doesn't set it, even though the sub stream's
/// pixels aren't square. So define a default based on the pixel dimensions to use if the camera
/// doesn't tell us what to do.
///
/// Note that at least in the case of .mp4 muxing, we don't need to fix up the underlying SPS.
/// SPS; PixelAspectRatioBox's definition says that it overrides the H.264-level declaration.
fn default_pixel_aspect_ratio(width: u16, height: u16) -> (u16, u16) {
    let dims = (width, height);
    for r in &PIXEL_ASPECT_RATIOS {
        if r.0 == dims {
            return r.1;
        }
    }
    (1, 1)
}

/// Decodes a H.264 Annex B byte stream into NAL units. Calls `f` for each NAL unit in the byte
/// stream. Aborts if `f` returns error.
///
/// Note `f` is called with the encoded NAL form, not the RBSP. The NAL header byte and any
/// emulation prevention bytes will be present.
///
/// See ISO/IEC 14496-10 section B.2: Byte stream NAL unit decoding process.
/// This is a relatively simple, unoptimized implementation.
///
/// TODO: detect invalid byte streams. For example, several 0x00s not followed by a 0x01, a stream
/// stream not starting with 0x00 0x00 0x00 0x01, or an empty NAL unit.
fn decode_h264_annex_b<'a, F>(mut data: &'a [u8], mut f: F) -> Result<(), Error>
where
    F: FnMut(&'a [u8]) -> Result<(), Error>,
{
    let start_code = &b"\x00\x00\x01"[..];
    use nom::FindSubstring;
    'outer: while let Some(pos) = data.find_substring(start_code) {
        let mut unit = &data[0..pos];
        data = &data[pos + start_code.len()..];
        // Have zero or more bytes that end in a start code. Strip out any trailing 0x00s and
        // process the unit if there's anything left.
        loop {
            match unit.last() {
                None => continue 'outer,
                Some(b) if *b == 0 => {
                    unit = &unit[..unit.len() - 1];
                }
                Some(_) => break,
            }
        }
        f(unit)?;
    }

    // No remaining start codes; likely a unit left.
    if !data.is_empty() {
        f(data)?;
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

/// Decodes a NAL unit (minus header byte) into its RBSP.
/// Stolen from h264-reader's src/avcc.rs. This shouldn't last long, see:
/// <https://github.com/dholroyd/h264-reader/issues/4>.
fn decode(encoded: &[u8]) -> Vec<u8> {
    struct NalRead(Vec<u8>);
    use h264_reader::nal::NalHandler;
    use h264_reader::Context;
    impl NalHandler for NalRead {
        type Ctx = ();
        fn start(&mut self, _ctx: &mut Context<Self::Ctx>, _header: h264_reader::nal::NalHeader) {}

        fn push(&mut self, _ctx: &mut Context<Self::Ctx>, buf: &[u8]) {
            self.0.extend_from_slice(buf)
        }

        fn end(&mut self, _ctx: &mut Context<Self::Ctx>) {}
    }
    let mut decode = h264_reader::rbsp::RbspDecoder::new(NalRead(vec![]));
    let mut ctx = Context::new(());
    decode.push(&mut ctx, encoded);
    let read = decode.into_handler();
    read.0
}

/// Parsed representation of ffmpeg's "extradata".
#[derive(Debug, PartialEq, Eq)]
pub struct ExtraData {
    pub entry: db::VideoSampleEntryToInsert,

    /// True iff sample data should be transformed from Annex B format to AVC format via a call to
    /// `transform_sample_data`. (The assumption is that if the extra data was in Annex B format,
    /// the sample data is also.)
    pub need_transform: bool,
}

impl ExtraData {
    /// Parses "extradata" from ffmpeg. This data may be in either Annex B format or AVC format.
    pub fn parse(extradata: &[u8], width: u16, height: u16) -> Result<ExtraData, Error> {
        let raw_sps_and_pps;
        let need_transform;
        let ctx;
        let sps_owner;
        let sps; // reference to either within ctx or to sps_owner.
        if extradata.starts_with(b"\x00\x00\x00\x01") || extradata.starts_with(b"\x00\x00\x01") {
            // ffmpeg supplied "extradata" in Annex B format.
            let (s, p) = parse_annex_b_extra_data(extradata)?;
            let rbsp = decode(&s[1..]);
            sps_owner = h264_reader::nal::sps::SeqParameterSet::from_bytes(&rbsp)
                .map_err(|e| format_err!("Bad SPS: {:?}", e))?;
            sps = &sps_owner;
            raw_sps_and_pps = Some((s, p));
            need_transform = true;
        } else {
            // Assume "extradata" holds an AVCDecoderConfiguration.
            need_transform = false;
            raw_sps_and_pps = None;
            let avcc = h264_reader::avcc::AvcDecoderConfigurationRecord::try_from(extradata)
                .map_err(|e| format_err!("Bad AvcDecoderConfigurationRecord: {:?}", e))?;
            if avcc.num_of_sequence_parameter_sets() != 1 {
                bail!("Multiple SPSs!");
            }
            ctx = avcc
                .create_context(())
                .map_err(|e| format_err!("Can't load SPS+PPS: {:?}", e))?;
            sps = ctx
                .sps_by_id(h264_reader::nal::pps::ParamSetId::from_u32(0).unwrap())
                .ok_or_else(|| format_err!("No SPS 0"))?;
        };

        let mut sample_entry = Vec::with_capacity(256);

        // This is a concatenation of the following boxes/classes.

        // SampleEntry, ISO/IEC 14496-12 section 8.5.2.
        let avc1_len_pos = sample_entry.len();
        // length placeholder + type + reserved + data_reference_index = 1
        sample_entry.extend_from_slice(b"\x00\x00\x00\x00avc1\x00\x00\x00\x00\x00\x00\x00\x01");

        // VisualSampleEntry, ISO/IEC 14496-12 section 12.1.3.
        sample_entry.extend_from_slice(&[0; 16]); // pre-defined + reserved
        sample_entry.write_u16::<BigEndian>(width)?;
        sample_entry.write_u16::<BigEndian>(height)?;
        sample_entry.extend_from_slice(&[
            0x00, 0x48, 0x00, 0x00, // horizresolution
            0x00, 0x48, 0x00, 0x00, // vertresolution
            0x00, 0x00, 0x00, 0x00, // reserved
            0x00, 0x01, // frame count
            0x00, 0x00, 0x00, 0x00, // compressorname
            0x00, 0x00, 0x00, 0x00, //
            0x00, 0x00, 0x00, 0x00, //
            0x00, 0x00, 0x00, 0x00, //
            0x00, 0x00, 0x00, 0x00, //
            0x00, 0x00, 0x00, 0x00, //
            0x00, 0x00, 0x00, 0x00, //
            0x00, 0x00, 0x00, 0x00, //
            0x00, 0x18, 0xff, 0xff, // depth + pre_defined
        ]);

        // AVCSampleEntry, ISO/IEC 14496-15 section 5.3.4.1.
        // AVCConfigurationBox, ISO/IEC 14496-15 section 5.3.4.1.
        let avcc_len_pos = sample_entry.len();
        sample_entry.extend_from_slice(b"\x00\x00\x00\x00avcC");

        if let Some((sps, pps)) = raw_sps_and_pps {
            // Create the AVCDecoderConfiguration, ISO/IEC 14496-15 section 5.2.4.1.
            // The beginning of the AVCDecoderConfiguration takes a few values from
            // the SPS (ISO/IEC 14496-10 section 7.3.2.1.1). One caveat: that section
            // defines the syntax in terms of RBSP, not NAL. The difference is the
            // escaping of 00 00 01 and 00 00 02; see notes about
            // "emulation_prevention_three_byte" in ISO/IEC 14496-10 section 7.4.
            // It looks like 00 is not a valid value of profile_idc, so this distinction
            // shouldn't be relevant here. And ffmpeg seems to ignore it.
            sample_entry.push(1); // configurationVersion
            sample_entry.push(sps[1]); // profile_idc . AVCProfileIndication
            sample_entry.push(sps[2]); // ...misc bits... . profile_compatibility
            sample_entry.push(sps[3]); // level_idc . AVCLevelIndication

            // Hardcode lengthSizeMinusOne to 3, matching TransformSampleData's 4-byte
            // lengths.
            sample_entry.push(0xff);

            // Only support one SPS and PPS.
            // ffmpeg's ff_isom_write_avcc has the same limitation, so it's probably
            // fine. This next byte is a reserved 0b111 + a 5-bit # of SPSs (1).
            sample_entry.push(0xe1);
            sample_entry.write_u16::<BigEndian>(u16::try_from(sps.len())?)?;
            sample_entry.extend_from_slice(sps);
            sample_entry.push(1); // # of PPSs.
            sample_entry.write_u16::<BigEndian>(u16::try_from(pps.len())?)?;
            sample_entry.extend_from_slice(pps);
        } else {
            sample_entry.extend_from_slice(extradata);
        };

        // Fix up avc1 and avcC box lengths.
        let cur_pos = sample_entry.len();
        BigEndian::write_u32(
            &mut sample_entry[avcc_len_pos..avcc_len_pos + 4],
            u32::try_from(cur_pos - avcc_len_pos)?,
        );

        // PixelAspectRatioBox, ISO/IEC 14496-12 section 12.1.4.2.
        // Write a PixelAspectRatioBox if necessary, as the sub streams can be be anamorphic.
        let pasp = sps
            .vui_parameters
            .as_ref()
            .and_then(|v| v.aspect_ratio_info.as_ref())
            .and_then(|a| a.clone().get())
            .unwrap_or_else(|| default_pixel_aspect_ratio(width, height));
        if pasp != (1, 1) {
            sample_entry.extend_from_slice(b"\x00\x00\x00\x10pasp"); // length + box name
            sample_entry.write_u32::<BigEndian>(pasp.0.into())?;
            sample_entry.write_u32::<BigEndian>(pasp.1.into())?;
        }

        let cur_pos = sample_entry.len();
        BigEndian::write_u32(
            &mut sample_entry[avc1_len_pos..avc1_len_pos + 4],
            u32::try_from(cur_pos - avc1_len_pos)?,
        );

        let profile_idc = sample_entry[103];
        let constraint_flags = sample_entry[104];
        let level_idc = sample_entry[105];

        let rfc6381_codec = format!(
            "avc1.{:02x}{:02x}{:02x}",
            profile_idc, constraint_flags, level_idc
        );
        Ok(ExtraData {
            entry: db::VideoSampleEntryToInsert {
                data: sample_entry,
                rfc6381_codec,
                width,
                height,
                pasp_h_spacing: pasp.0,
                pasp_v_spacing: pasp.1,
            },
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
        avc_sample.write_u32::<BigEndian>(unit.len() as u32)?; // length
        avc_sample.extend_from_slice(unit);
        Ok(())
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use db::testutil;

    #[rustfmt::skip]
    const ANNEX_B_TEST_INPUT: [u8; 35] = [
        0x00, 0x00, 0x00, 0x01, 0x67, 0x4d, 0x00, 0x1f,
        0x9a, 0x66, 0x02, 0x80, 0x2d, 0xff, 0x35, 0x01,
        0x01, 0x01, 0x40, 0x00, 0x00, 0xfa, 0x00, 0x00,
        0x1d, 0x4c, 0x01, 0x00, 0x00, 0x00, 0x01, 0x68,
        0xee, 0x3c, 0x80,
    ];

    #[rustfmt::skip]
    const AVC_DECODER_CONFIG_TEST_INPUT: [u8; 38] = [
        0x01, 0x4d, 0x00, 0x1f, 0xff, 0xe1, 0x00, 0x17,
        0x67, 0x4d, 0x00, 0x1f, 0x9a, 0x66, 0x02, 0x80,
        0x2d, 0xff, 0x35, 0x01, 0x01, 0x01, 0x40, 0x00,
        0x00, 0xfa, 0x00, 0x00, 0x1d, 0x4c, 0x01, 0x01,
        0x00, 0x04, 0x68, 0xee, 0x3c, 0x80,
    ];

    #[rustfmt::skip]
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
        })
        .unwrap();
        assert_eq!(&pieces, &[&data[4..27], &data[31..]]);
    }

    #[test]
    fn test_sample_entry_from_avc_decoder_config() {
        testutil::init();
        let e = super::ExtraData::parse(&AVC_DECODER_CONFIG_TEST_INPUT, 1280, 720).unwrap();
        assert_eq!(&e.entry.data[..], &TEST_OUTPUT[..]);
        assert_eq!(e.entry.width, 1280);
        assert_eq!(e.entry.height, 720);
        assert_eq!(e.entry.rfc6381_codec, "avc1.4d001f");
        assert_eq!(e.need_transform, false);
    }

    #[test]
    fn test_sample_entry_from_annex_b() {
        testutil::init();
        let e = super::ExtraData::parse(&ANNEX_B_TEST_INPUT, 1280, 720).unwrap();
        assert_eq!(e.entry.width, 1280);
        assert_eq!(e.entry.height, 720);
        assert_eq!(e.entry.rfc6381_codec, "avc1.4d001f");
        assert_eq!(e.need_transform, true);
    }

    #[test]
    fn test_transform_sample_data() {
        testutil::init();
        #[rustfmt::skip]
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
        #[rustfmt::skip]
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
