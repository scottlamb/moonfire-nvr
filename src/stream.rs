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

use error::Error;
use ffmpeg::{self, format, media};
use ffmpeg_sys::{self, AVLockOp};
use h264;
use libc::{self, c_int, c_void};
use std::mem;
use std::ptr;
use std::result::Result;
use std::slice;
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

// TODO: I think this should be provided by ffmpeg-sys. Otherwise, ffmpeg-sys is thread-hostile,
// which I believe is not allowed at all in Rust. (Also, this method's signature should include
// unsafe.)
extern "C" fn lock_callback(untyped_ptr: *mut *mut c_void, op: AVLockOp) -> c_int {
    unsafe {
        let ptr = mem::transmute::<*mut *mut c_void, *mut *mut libc::pthread_mutex_t>(untyped_ptr);
        match op {
            AVLockOp::AV_LOCK_CREATE => {
                let m = Box::<libc::pthread_mutex_t>::new(mem::uninitialized());
                *ptr = Box::into_raw(m);
                libc::pthread_mutex_init(*ptr, ptr::null());
            },
            AVLockOp::AV_LOCK_DESTROY => {
                libc::pthread_mutex_destroy(*ptr);
                Box::from_raw(*ptr);  // delete.
                *ptr = ptr::null_mut();
            },
            AVLockOp::AV_LOCK_OBTAIN => {
                libc::pthread_mutex_lock(*ptr);
            },
            AVLockOp::AV_LOCK_RELEASE => {
                libc::pthread_mutex_unlock(*ptr);
            },
        };
    };
    0
}

pub trait Opener<S : Stream> : Sync {
    fn open(&self, src: Source) -> Result<S, Error>;
}

pub trait Stream {
    fn get_extra_data(&self) -> Result<h264::ExtraData, Error>;
    fn get_next(&mut self) -> Result<ffmpeg::Packet, ffmpeg::Error>;
}

pub struct Ffmpeg {}

impl Ffmpeg {
    fn new() -> Ffmpeg {
        START.call_once(|| {
            unsafe { ffmpeg_sys::av_lockmgr_register(lock_callback); };
            ffmpeg::init().unwrap();
            ffmpeg::format::network::init();
        });
        Ffmpeg{}
    }
}

impl Opener<FfmpegStream> for Ffmpeg {
    fn open(&self, src: Source) -> Result<FfmpegStream, Error> {
        let (input, discard_first) = match src {
            #[cfg(test)]
            Source::File(filename) =>
                (format::input_with(&format!("file:{}", filename), ffmpeg::Dictionary::new())?,
                 false),
            Source::Rtsp(url) => {
                let open_options = dict![
                    "rtsp_transport" => "tcp",
                    // https://trac.ffmpeg.org/ticket/5018 workaround attempt.
                    "probesize" => "262144",
                    "user-agent" => "moonfire-nvr",
                    // 10-second socket timeout, in microseconds.
                    "stimeout" => "10000000"
                ];
                (format::input_with(&url, open_options)?, true)
            },
        };

        // Find the video stream.
        let mut video_i = None;
        for (i, stream) in input.streams().enumerate() {
            if stream.codec().medium() == media::Type::Video {
                debug!("Video stream index is {}", i);
                video_i = Some(i);
                break;
            }
        }
        let video_i = match video_i {
            Some(i) => i,
            None => { return Err(Error::new("no video stream".to_owned())) },
        };

        let mut stream = FfmpegStream{
            input: input,
            video_i: video_i,
        };

        if discard_first {
            info!("Discarding the first packet to work around https://trac.ffmpeg.org/ticket/5018");
            stream.get_next()?;
        }

        Ok(stream)
    }
}

pub struct FfmpegStream {
    input: format::context::Input,
    video_i: usize,
}

impl Stream for FfmpegStream {
    fn get_extra_data(&self) -> Result<h264::ExtraData, Error> {
        let video = self.input.stream(self.video_i).expect("can't get video stream known to exist");
        let codec = video.codec();
        let (extradata, width, height) = unsafe {
            let ptr = codec.as_ptr();
            (slice::from_raw_parts((*ptr).extradata, (*ptr).extradata_size as usize),
             (*ptr).width as u16,
             (*ptr).height as u16)
        };
        // TODO: verify video stream is h264.
        h264::ExtraData::parse(extradata, width, height)
    }

    fn get_next(&mut self) -> Result<ffmpeg::Packet, ffmpeg::Error> {
        let mut pkt = ffmpeg::Packet::empty();
        loop {
            pkt.read(&mut self.input)?;
            if pkt.stream() == self.video_i {
                return Ok(pkt);
            }
        }
    }
}
