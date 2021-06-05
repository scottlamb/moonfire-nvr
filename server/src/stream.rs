// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2016 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

use crate::h264;
use cstr::cstr;
use failure::{bail, Error};
use lazy_static::lazy_static;
use log::warn;
use std::convert::TryFrom;
use std::ffi::CString;
use std::result::Result;

static START: parking_lot::Once = parking_lot::Once::new();

lazy_static! {
    pub static ref FFMPEG: Ffmpeg = Ffmpeg::new();
}

pub enum Source<'a> {
    /// A filename, for testing.
    #[cfg(test)]
    File(&'a str),

    /// An RTSP stream, for production use.
    Rtsp { url: &'a str, redacted_url: &'a str },
}

pub trait Opener<S: Stream>: Sync {
    fn open(&self, src: Source) -> Result<S, Error>;
}

pub trait Stream {
    fn get_video_codecpar(&self) -> ffmpeg::avcodec::InputCodecParameters<'_>;
    fn get_extra_data(&self) -> Result<h264::ExtraData, Error>;
    fn get_next(&mut self) -> Result<ffmpeg::avcodec::Packet, ffmpeg::Error>;
}

pub struct Ffmpeg {}

impl Ffmpeg {
    fn new() -> Ffmpeg {
        START.call_once(|| {
            ffmpeg::Ffmpeg::new();
        });
        Ffmpeg {}
    }
}

impl Opener<FfmpegStream> for Ffmpeg {
    fn open(&self, src: Source) -> Result<FfmpegStream, Error> {
        use ffmpeg::avformat::InputFormatContext;
        let mut input = match src {
            #[cfg(test)]
            Source::File(filename) => {
                let mut open_options = ffmpeg::avutil::Dictionary::new();

                // Work around https://github.com/scottlamb/moonfire-nvr/issues/10
                open_options
                    .set(cstr!("advanced_editlist"), cstr!("false"))
                    .unwrap();
                let url = format!("file:{}", filename);
                let i = InputFormatContext::open(
                    &CString::new(url.clone()).unwrap(),
                    &mut open_options,
                )?;
                if !open_options.empty() {
                    warn!(
                        "While opening URL {}, some options were not understood: {}",
                        url, open_options
                    );
                }
                i
            }
            Source::Rtsp { url, redacted_url } => {
                let mut open_options = ffmpeg::avutil::Dictionary::new();
                open_options
                    .set(cstr!("rtsp_transport"), cstr!("tcp"))
                    .unwrap();
                open_options
                    .set(cstr!("user-agent"), cstr!("moonfire-nvr"))
                    .unwrap();

                // 10-second socket timeout, in microseconds.
                open_options
                    .set(cstr!("stimeout"), cstr!("10000000"))
                    .unwrap();

                // Without this option, the first packet has an incorrect pts.
                // https://trac.ffmpeg.org/ticket/5018
                open_options
                    .set(cstr!("fflags"), cstr!("nobuffer"))
                    .unwrap();

                // Moonfire NVR currently only supports video, so receiving audio is wasteful.
                // It also triggers <https://github.com/scottlamb/moonfire-nvr/issues/36>.
                open_options
                    .set(cstr!("allowed_media_types"), cstr!("video"))
                    .unwrap();

                let i = InputFormatContext::open(&CString::new(url).unwrap(), &mut open_options)?;
                if !open_options.empty() {
                    warn!(
                        "While opening URL {}, some options were not understood: {}",
                        redacted_url, open_options
                    );
                }
                i
            }
        };

        input.find_stream_info()?;

        // Find the video stream.
        let mut video_i = None;
        {
            let s = input.streams();
            for i in 0..s.len() {
                if s.get(i).codecpar().codec_type().is_video() {
                    video_i = Some(i);
                    break;
                }
            }
        }
        let video_i = match video_i {
            Some(i) => i,
            None => bail!("no video stream"),
        };

        Ok(FfmpegStream { input, video_i })
    }
}

pub struct FfmpegStream {
    input: ffmpeg::avformat::InputFormatContext<'static>,
    video_i: usize,
}

impl Stream for FfmpegStream {
    fn get_video_codecpar(&self) -> ffmpeg::avcodec::InputCodecParameters {
        self.input.streams().get(self.video_i).codecpar()
    }

    fn get_extra_data(&self) -> Result<h264::ExtraData, Error> {
        let video = self.input.streams().get(self.video_i);
        let tb = video.time_base();
        if tb.num != 1 || tb.den != 90000 {
            bail!(
                "video stream has timebase {}/{}; expected 1/90000",
                tb.num,
                tb.den
            );
        }
        let codec = video.codecpar();
        let codec_id = codec.codec_id();
        if !codec_id.is_h264() {
            bail!("stream's video codec {:?} is not h264", codec_id);
        }
        let dims = codec.dims();
        h264::ExtraData::parse(
            codec.extradata(),
            u16::try_from(dims.width)?,
            u16::try_from(dims.height)?,
        )
    }

    fn get_next(&mut self) -> Result<ffmpeg::avcodec::Packet, ffmpeg::Error> {
        loop {
            let p = self.input.read_frame()?;
            if p.stream_index() == self.video_i {
                return Ok(p);
            }
        }
    }
}
