// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! H.264 decoding
//!
//! For the most part, Moonfire NVR does not try to understand the video codec. However, H.264 has
//! two byte stream encodings: ISO/IEC 14496-10 Annex B, and ISO/IEC 14496-15 AVC access units.
//! When streaming from RTSP, ffmpeg supplies the former. We need the latter to stick into `.mp4`
//! files. This file manages the conversion, both for the ffmpeg "extra data" (which should become
//! the ISO/IEC 14496-15 section 5.2.4.1 `AVCDecoderConfigurationRecord`) and the actual samples.
//!
//! See the [wiki page on standards and
//! specifications](https://github.com/scottlamb/moonfire-nvr/wiki/Standards-and-specifications)
//! for help finding a copy of the relevant standards. This code won't make much sense without them!
//!
//! ffmpeg of course has logic to do the same thing, but unfortunately it is not exposed except
//! through ffmpeg's own generated `.mp4` file. Extracting just this part of their `.mp4` files
//! would be more trouble than it's worth.

use base::{bail, err, Error};
use byteorder::{BigEndian, ByteOrder, WriteBytesExt};
use db::VideoSampleEntryToInsert;
use h264_reader::nal::Nal;
use pretty_hex::PrettyHex as _;
use std::convert::TryFrom;

// For certain common sub stream anamorphic resolutions, add a pixel aspect ratio box.
// Assume the camera is 16x9. These are just the standard wide mode; default_pixel_aspect_ratio
// tries the transpose also.
const PIXEL_ASPECT_RATIOS: [((u16, u16), (u16, u16)); 6] = [
    ((320, 240), (4, 3)),
    ((352, 240), (40, 33)),
    ((640, 352), (44, 45)),
    ((640, 480), (4, 3)),
    ((704, 480), (40, 33)),
    ((720, 480), (32, 27)),
];

/// Get the pixel aspect ratio to use if none is specified.
///
/// The Dahua IPC-HDW5231R-Z sets the aspect ratio in the H.264 SPS (correctly) for both square and
/// non-square pixels. The Hikvision DS-2CD2032-I doesn't set it, even though the sub stream's
/// pixels aren't square. So define a default based on the pixel dimensions to use if the camera
/// doesn't tell us what to do.
///
/// Note that at least in the case of .mp4 muxing, we don't need to fix up the underlying SPS.
/// PixelAspectRatioBox's definition says that it overrides the H.264-level declaration.
fn default_pixel_aspect_ratio(width: u16, height: u16) -> (u16, u16) {
    if width >= height {
        PIXEL_ASPECT_RATIOS
            .iter()
            .find(|r| r.0 == (width, height))
            .map(|r| r.1)
            .unwrap_or((1, 1))
    } else {
        PIXEL_ASPECT_RATIOS
            .iter()
            .find(|r| r.0 == (height, width))
            .map(|r| (r.1 .1, r.1 .0))
            .unwrap_or((1, 1))
    }
}

/// `h264_reader::rbsp::BitRead` impl that does not care about extra trailing data.
///
/// Some (Reolink) cameras appear to have a stray extra byte at the end. Follow the lead of most
/// other RTSP implementations in tolerating this.
#[derive(Debug)]
struct TolerantBitReader<R> {
    inner: R,
}

impl<R: h264_reader::rbsp::BitRead> h264_reader::rbsp::BitRead for TolerantBitReader<R> {
    fn read_ue(&mut self, name: &'static str) -> Result<u32, h264_reader::rbsp::BitReaderError> {
        self.inner.read_ue(name)
    }

    fn read_se(&mut self, name: &'static str) -> Result<i32, h264_reader::rbsp::BitReaderError> {
        self.inner.read_se(name)
    }

    fn read_bool(&mut self, name: &'static str) -> Result<bool, h264_reader::rbsp::BitReaderError> {
        self.inner.read_bool(name)
    }

    fn read_u8(
        &mut self,
        bit_count: u32,
        name: &'static str,
    ) -> Result<u8, h264_reader::rbsp::BitReaderError> {
        self.inner.read_u8(bit_count, name)
    }

    fn read_u16(
        &mut self,
        bit_count: u32,
        name: &'static str,
    ) -> Result<u16, h264_reader::rbsp::BitReaderError> {
        self.inner.read_u16(bit_count, name)
    }

    fn read_u32(
        &mut self,
        bit_count: u32,
        name: &'static str,
    ) -> Result<u32, h264_reader::rbsp::BitReaderError> {
        self.inner.read_u32(bit_count, name)
    }

    fn read_i32(
        &mut self,
        bit_count: u32,
        name: &'static str,
    ) -> Result<i32, h264_reader::rbsp::BitReaderError> {
        self.inner.read_i32(bit_count, name)
    }

    fn has_more_rbsp_data(
        &mut self,
        name: &'static str,
    ) -> Result<bool, h264_reader::rbsp::BitReaderError> {
        self.inner.has_more_rbsp_data(name)
    }

    fn finish_rbsp(self) -> Result<(), h264_reader::rbsp::BitReaderError> {
        match self.inner.finish_rbsp() {
            Ok(()) => Ok(()),
            Err(h264_reader::rbsp::BitReaderError::RemainingData) => {
                tracing::debug!("extra data at end of NAL unit");
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    fn finish_sei_payload(self) -> Result<(), h264_reader::rbsp::BitReaderError> {
        self.inner.finish_sei_payload()
    }
}

fn parse_extra_data_inner(extradata: &[u8]) -> Result<VideoSampleEntryToInsert, Error> {
    let avcc =
        h264_reader::avcc::AvcDecoderConfigurationRecord::try_from(extradata).map_err(|e| {
            err!(
                InvalidArgument,
                msg("bad AvcDecoderConfigurationRecord: {:?}", e)
            )
        })?;
    if avcc.num_of_sequence_parameter_sets() != 1 {
        bail!(Unimplemented, msg("multiple SPSs!"));
    }

    // This logic is essentially copied from
    // `h264_reader::avcc::AvcDecoderConfigurationRecord::create_context` but
    // using our `TolerantBitReader` wrapper.
    let mut ctx = h264_reader::Context::new();
    for sps in avcc.sequence_parameter_sets() {
        let sps = h264_reader::nal::RefNal::new(
            &sps.map_err(|e| err!(InvalidArgument, msg("bad SPS: {e:?}")))?[..],
            &[],
            true,
        );
        let sps = h264_reader::nal::sps::SeqParameterSet::from_bits(TolerantBitReader {
            inner: sps.rbsp_bits(),
        })
        .map_err(|e| err!(InvalidArgument, msg("bad SPS: {e:?}")))?;
        ctx.put_seq_param_set(sps);
    }
    for pps in avcc.picture_parameter_sets() {
        let pps = h264_reader::nal::RefNal::new(
            &pps.map_err(|e| err!(InvalidArgument, msg("bad PPS: {e:?}")))?[..],
            &[],
            true,
        );
        let pps = h264_reader::nal::pps::PicParameterSet::from_bits(
            &ctx,
            TolerantBitReader {
                inner: pps.rbsp_bits(),
            },
        )
        .map_err(|e| err!(InvalidArgument, msg("bad PPS: {e:?}")))?;
        ctx.put_pic_param_set(pps);
    }

    let sps = ctx
        .sps_by_id(h264_reader::nal::pps::ParamSetId::from_u32(0).unwrap())
        .ok_or_else(|| err!(Unimplemented, msg("no SPS 0")))?;
    let pixel_dimensions = sps.pixel_dimensions().map_err(|e| {
        err!(
            InvalidArgument,
            msg("SPS has invalid pixel dimensions: {:?}", e)
        )
    })?;
    let (Ok(width), Ok(height)) = (
        u16::try_from(pixel_dimensions.0),
        u16::try_from(pixel_dimensions.1),
    ) else {
        bail!(
            InvalidArgument,
            msg(
                "bad dimensions {}x{}",
                pixel_dimensions.0,
                pixel_dimensions.1
            )
        );
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
    sample_entry.extend_from_slice(extradata);

    // Fix up avc1 and avcC box lengths.
    let cur_pos = sample_entry.len();
    BigEndian::write_u32(
        &mut sample_entry[avcc_len_pos..avcc_len_pos + 4],
        u32::try_from(cur_pos - avcc_len_pos).map_err(|_| err!(OutOfRange))?,
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
        u32::try_from(cur_pos - avc1_len_pos).map_err(|_| err!(OutOfRange))?,
    );

    let profile_idc = sample_entry[103];
    let constraint_flags = sample_entry[104];
    let level_idc = sample_entry[105];

    let rfc6381_codec = format!("avc1.{profile_idc:02x}{constraint_flags:02x}{level_idc:02x}");
    Ok(VideoSampleEntryToInsert {
        data: sample_entry,
        rfc6381_codec,
        width,
        height,
        pasp_h_spacing: pasp.0,
        pasp_v_spacing: pasp.1,
    })
}

/// Parses the `AvcDecoderConfigurationRecord` in the "extra data".
pub fn parse_extra_data(extradata: &[u8]) -> Result<VideoSampleEntryToInsert, Error> {
    parse_extra_data_inner(extradata).map_err(|e| {
        err!(
            e,
            msg(
                "can't parse AvcDecoderRecord {}",
                extradata.hex_conf(pretty_hex::HexConfig {
                    width: 0,
                    group: 0,
                    chunk: 0,
                    ..Default::default()
                })
            )
        )
    })
}

#[cfg(test)]
mod tests {
    use db::testutil;

    #[rustfmt::skip]
    const AVC_DECODER_CONFIG_TEST_INPUT: [u8; 38] = [
        0x01, 0x4d, 0x00, 0x1f, 0xff,

        0xe1, 0x00, 0x17, // 1 SPS, length 0x17
        0x67, 0x4d, 0x00, 0x1f, 0x9a, 0x66, 0x02, 0x80,
        0x2d, 0xff, 0x35, 0x01, 0x01, 0x01, 0x40, 0x00,
        0x00, 0xfa, 0x00, 0x00, 0x1d, 0x4c, 0x01,

        0x01, 0x00, 0x04, // 1 PPS, length 0x04
        0x68, 0xee, 0x3c, 0x80,
    ];

    #[rustfmt::skip]
    const AVC_DECODER_CONFIG_TEST_INPUT_WITH_TRAILING_GARBAGE: [u8; 40] = [
        0x01, 0x4d, 0x00, 0x1f, 0xff,

        0xe1, 0x00, 0x18, // 1 SPS, length 0x18
        0x67, 0x4d, 0x00, 0x1f, 0x9a, 0x66, 0x02, 0x80,
        0x2d, 0xff, 0x35, 0x01, 0x01, 0x01, 0x40, 0x00,
        0x00, 0xfa, 0x00, 0x00, 0x1d, 0x4c, 0x01, 0x01,

        0x01, 0x00, 0x04, // 1 PPS, length 0x05
        0x68, 0xee, 0x3c, 0x80, 0x80,
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
    fn test_sample_entry_from_avc_decoder_config() {
        testutil::init();
        let e = super::parse_extra_data(&AVC_DECODER_CONFIG_TEST_INPUT).unwrap();
        assert_eq!(&e.data[..], &TEST_OUTPUT[..]);
        assert_eq!(e.width, 1280);
        assert_eq!(e.height, 720);
        assert_eq!(e.rfc6381_codec, "avc1.4d001f");
    }

    #[test]
    fn pixel_aspect_ratios() {
        use super::default_pixel_aspect_ratio;
        use num_rational::Ratio;
        for &((w, h), _) in &super::PIXEL_ASPECT_RATIOS {
            let (h_spacing, v_spacing) = default_pixel_aspect_ratio(w, h);
            assert_eq!(Ratio::new(w * h_spacing, h * v_spacing), Ratio::new(16, 9));

            // 90 or 270 degree rotation.
            let (h_spacing, v_spacing) = default_pixel_aspect_ratio(h, w);
            assert_eq!(Ratio::new(h * h_spacing, w * v_spacing), Ratio::new(9, 16));
        }
    }

    #[test]
    fn extra_sps_data() {
        super::parse_extra_data(&AVC_DECODER_CONFIG_TEST_INPUT_WITH_TRAILING_GARBAGE).unwrap();
    }
}
