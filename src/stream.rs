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

use crate::h264;
use failure::{Error, bail};
use ffmpeg;
use lazy_static::lazy_static;
use log::{debug, info, warn};
use std::os::raw::c_char;
use std::ffi::{CStr, CString};
use std::result::Result;
use std::sync;

static START: sync::Once = sync::ONCE_INIT;

lazy_static! {
    pub static ref FFMPEG: Ffmpeg = Ffmpeg::new();
}

pub enum Source<'a> {
    #[cfg(test)]
    File(&'a str),  // filename, for testing.

    Rtsp(&'a str),  // url, for production use.
}

pub trait Opener<S : Stream> : Sync {
    fn open(&self, src: Source) -> Result<S, Error>;
}

pub trait Stream {
    fn get_extra_data(&self) -> Result<h264::ExtraData, Error>;
    fn get_next<'p>(&'p mut self) -> Result<ffmpeg::Packet<'p>, ffmpeg::Error>;
}

pub struct Ffmpeg {}

impl Ffmpeg {
    fn new() -> Ffmpeg {
        START.call_once(|| {
            ffmpeg::Ffmpeg::new();
            //ffmpeg::init().unwrap();
            //ffmpeg::format::network::init();
        });
        Ffmpeg{}
    }
}

macro_rules! c_str {
    ($s:expr) => { {
        unsafe { CStr::from_ptr(concat!($s, "\0").as_ptr() as *const c_char) }
    } }
}

impl Opener<FfmpegStream> for Ffmpeg {
    fn open(&self, src: Source) -> Result<FfmpegStream, Error> {
        use ffmpeg::InputFormatContext;
        let (mut input, discard_first) = match src {
            #[cfg(test)]
            Source::File(filename) => {
                let mut open_options = ffmpeg::Dictionary::new();

                // Work around https://github.com/scottlamb/moonfire-nvr/issues/10
                open_options.set(c_str!("advanced_editlist"), c_str!("false")).unwrap();
                let url = format!("file:{}", filename);
                let i = InputFormatContext::open(&CString::new(url.clone()).unwrap(),
                                                 &mut open_options)?;
                if !open_options.empty() {
                    warn!("While opening URL {}, some options were not understood: {}",
                          url, open_options);
                }
                (i, false)
            }
            Source::Rtsp(url) => {
                let mut open_options = ffmpeg::Dictionary::new();
                open_options.set(c_str!("rtsp_transport"), c_str!("tcp")).unwrap();
                open_options.set(c_str!("user-agent"), c_str!("moonfire-nvr")).unwrap();
                // 10-second socket timeout, in microseconds.
                open_options.set(c_str!("stimeout"), c_str!("10000000")).unwrap();

                // Moonfire NVR currently only supports video, so receiving audio is wasteful.
                // It also triggers <https://github.com/scottlamb/moonfire-nvr/issues/36>.
                open_options.set(c_str!("allowed_media_types"), c_str!("video")).unwrap();

                let i = InputFormatContext::open(&CString::new(url).unwrap(), &mut open_options)?;
                if !open_options.empty() {
                    warn!("While opening URL {}, some options were not understood: {}",
                          url, open_options);
                }
                (i, true)
            },
        };

        input.find_stream_info()?;

        // Find the video stream.
        let mut video_i = None;
        {
            let s = input.streams();
            for i in 0 .. s.len() {
                if s.get(i).codec().codec_type().is_video() {
                    debug!("Video stream index is {}", i);
                    video_i = Some(i);
                    break;
                }
            }
        }
        let video_i = match video_i {
            Some(i) => i,
            None => bail!("no video stream"),
        };

        let mut stream = FfmpegStream{
            input,
            video_i,
        };

        if discard_first {
            info!("Discarding the first packet to work around https://trac.ffmpeg.org/ticket/5018");
            stream.get_next()?;
        }

        Ok(stream)
    }
}

pub struct FfmpegStream {
    input: ffmpeg::InputFormatContext,
    video_i: usize,
}

impl Stream for FfmpegStream {
    fn get_extra_data(&self) -> Result<h264::ExtraData, Error> {
        let video = self.input.streams().get(self.video_i);
        let tb = video.time_base();
        if tb.num != 1 || tb.den != 90000 {
            bail!("video stream has timebase {}/{}; expected 1/90000", tb.num, tb.den);
        }
        let codec = video.codec();
        let codec_id = codec.codec_id();
        if !codec_id.is_h264() {
            bail!("stream's video codec {:?} is not h264", codec_id);
        }
        h264::ExtraData::parse(codec.extradata(), codec.width() as u16, codec.height() as u16)
    }

    fn get_next<'i>(&'i mut self) -> Result<ffmpeg::Packet<'i>, ffmpeg::Error> {
        loop {
            let p = self.input.read_frame()?;
            if p.stream_index() == self.video_i {
                return Ok(p);
            }
        }
    }
}
