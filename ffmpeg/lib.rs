// This file is part of Moonfire NVR, a security camera digital video recorder.
// Copyright (C) 2017 Scott Lamb <slamb@slamb.org>
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

use log::info;
use std::cell::{Ref, RefCell};
use std::ffi::CStr;
use std::fmt::{self, Write};
use std::ptr;
use std::sync;

static START: sync::Once = sync::ONCE_INIT;

//#[link(name = "avcodec")]
extern "C" {
    fn avcodec_version() -> libc::c_int;
    fn av_init_packet(p: *mut AVPacket);
    fn av_packet_unref(p: *mut AVPacket);

    fn moonfire_ffmpeg_cctx_codec_id(ctx: *const AVCodecContext) -> libc::c_int;
    fn moonfire_ffmpeg_cctx_codec_type(ctx: *const AVCodecContext) -> libc::c_int;
    fn moonfire_ffmpeg_cctx_extradata(ctx: *const AVCodecContext) -> DataLen;
    fn moonfire_ffmpeg_cctx_height(ctx: *const AVCodecContext) -> libc::c_int;
    fn moonfire_ffmpeg_cctx_width(ctx: *const AVCodecContext) -> libc::c_int;
}

//#[link(name = "avformat")]
extern "C" {
    fn avformat_version() -> libc::c_int;

    fn avformat_open_input(ctx: *mut *mut AVFormatContext, url: *const libc::c_char,
                           fmt: *const AVInputFormat, options: *mut *mut AVDictionary)
                           -> libc::c_int;
    fn avformat_close_input(ctx: *mut *mut AVFormatContext);
    fn avformat_find_stream_info(ctx: *mut AVFormatContext, options: *mut *mut AVDictionary)
                                 -> libc::c_int;
    fn av_read_frame(ctx: *mut AVFormatContext, p: *mut AVPacket) -> libc::c_int;
    fn av_register_all();
    fn avformat_network_init() -> libc::c_int;

    fn moonfire_ffmpeg_fctx_streams(ctx: *const AVFormatContext) -> StreamsLen;

    fn moonfire_ffmpeg_stream_codec(stream: *const AVStream) -> *const AVCodecContext;
    fn moonfire_ffmpeg_stream_time_base(stream: *const AVStream) -> AVRational;
}

//#[link(name = "avutil")]
extern "C" {
    fn avutil_version() -> libc::c_int;
    fn av_strerror(e: libc::c_int, b: *mut libc::c_char, s: libc::size_t) -> libc::c_int;
    fn av_dict_count(d: *const AVDictionary) -> libc::c_int;
    fn av_dict_get(d: *const AVDictionary, key: *const libc::c_char, prev: *mut AVDictionaryEntry,
                   flags: libc::c_int) -> *mut AVDictionaryEntry;
    fn av_dict_set(d: *mut *mut AVDictionary, key: *const libc::c_char, value: *const libc::c_char,
                   flags: libc::c_int) -> libc::c_int;
    fn av_dict_free(d: *mut *mut AVDictionary);
}

//#[link(name = "wrapper")]
extern "C" {
    static moonfire_ffmpeg_compiled_libavcodec_version: libc::c_int;
    static moonfire_ffmpeg_compiled_libavformat_version: libc::c_int;
    static moonfire_ffmpeg_compiled_libavutil_version: libc::c_int;
    static moonfire_ffmpeg_av_dict_ignore_suffix: libc::c_int;
    static moonfire_ffmpeg_av_nopts_value: libc::int64_t;

    static moonfire_ffmpeg_av_codec_id_h264: libc::c_int;
    static moonfire_ffmpeg_avmedia_type_video: libc::c_int;

    static moonfire_ffmpeg_averror_eof: libc::c_int;

    fn moonfire_ffmpeg_init();

    fn moonfire_ffmpeg_packet_alloc() -> *mut AVPacket;
    fn moonfire_ffmpeg_packet_free(p: *mut AVPacket);
    fn moonfire_ffmpeg_packet_is_key(p: *const AVPacket) -> bool;
    fn moonfire_ffmpeg_packet_pts(p: *const AVPacket) -> libc::int64_t;
    fn moonfire_ffmpeg_packet_dts(p: *const AVPacket) -> libc::int64_t;
    fn moonfire_ffmpeg_packet_duration(p: *const AVPacket) -> libc::c_int;
    fn moonfire_ffmpeg_packet_set_pts(p: *mut AVPacket, pts: libc::int64_t);
    fn moonfire_ffmpeg_packet_set_dts(p: *mut AVPacket, dts: libc::int64_t);
    fn moonfire_ffmpeg_packet_set_duration(p: *mut AVPacket, dur: libc::c_int);
    fn moonfire_ffmpeg_packet_data(p: *const AVPacket) -> DataLen;
    fn moonfire_ffmpeg_packet_stream_index(p: *const AVPacket) -> libc::c_uint;
}

pub struct Ffmpeg {}

// No accessors here; seems reasonable to assume ABI stability of this simple struct.
#[repr(C)]
struct AVDictionaryEntry {
    key: *mut libc::c_char,
    value: *mut libc::c_char,
}

// Likewise, seems reasonable to assume this struct has a stable ABI.
#[repr(C)]
pub struct AVRational {
    pub num: libc::c_int,
    pub den: libc::c_int,
}

// No ABI stability assumption here; use heap allocation/deallocation and accessors only.
enum AVCodecContext {}
enum AVDictionary {}
enum AVFormatContext {}
enum AVInputFormat {}
enum AVPacket {}
enum AVStream {}

pub struct InputFormatContext {
    ctx: *mut AVFormatContext,
    pkt: RefCell<*mut AVPacket>,
}

impl InputFormatContext {
    pub fn open(source: &CStr, dict: &mut Dictionary) -> Result<InputFormatContext, Error> {
        let mut ctx = ptr::null_mut();
        Error::wrap(unsafe {
            avformat_open_input(&mut ctx, source.as_ptr(), ptr::null(), &mut dict.0)
        })?;
        let pkt = unsafe { moonfire_ffmpeg_packet_alloc() };
        if pkt.is_null() {
            panic!("malloc failed");
        }
        unsafe { av_init_packet(pkt) };
        Ok(InputFormatContext{
            ctx,
            pkt: RefCell::new(pkt),
        })
    }

    pub fn find_stream_info(&mut self) -> Result<(), Error> {
        Error::wrap(unsafe { avformat_find_stream_info(self.ctx, ptr::null_mut()) })
    }

    // XXX: non-mut because of lexical lifetime woes in the caller. This is also why we need a
    // RefCell.
    pub fn read_frame<'i>(&'i self) -> Result<Packet<'i>, Error> {
        let pkt = self.pkt.borrow();
        Error::wrap(unsafe { av_read_frame(self.ctx, *pkt) })?;
        Ok(Packet(pkt))
    }

    pub fn streams<'i>(&'i self) -> Streams<'i> {
        Streams(unsafe {
            let s = moonfire_ffmpeg_fctx_streams(self.ctx);
            std::slice::from_raw_parts(s.streams, s.len as usize)
        })
    }
}

unsafe impl Send for InputFormatContext {}

impl Drop for InputFormatContext {
    fn drop(&mut self) {
        unsafe {
            moonfire_ffmpeg_packet_free(*self.pkt.borrow());
            avformat_close_input(&mut self.ctx);
        }
    }
}

// matches moonfire_ffmpeg_data_len
#[repr(C)]
struct DataLen {
    data: *const u8,
    len: libc::size_t,
}

// matches moonfire_ffmpeg_streams_len
#[repr(C)]
struct StreamsLen {
    streams: *const *const AVStream,
    len: libc::size_t,
}

pub struct Packet<'i>(Ref<'i, *mut AVPacket>);

impl<'i> Packet<'i> {
    pub fn is_key(&self) -> bool { unsafe { moonfire_ffmpeg_packet_is_key(*self.0) } }
    pub fn pts(&self) -> Option<i64> {
        match unsafe { moonfire_ffmpeg_packet_pts(*self.0) } {
            v if v == unsafe { moonfire_ffmpeg_av_nopts_value } => None,
            v => Some(v),
        }
    }
    pub fn set_pts(&mut self, pts: Option<i64>) {
        let real_pts = match pts {
            None => unsafe { moonfire_ffmpeg_av_nopts_value },
            Some(v) => v,
        };
        unsafe { moonfire_ffmpeg_packet_set_pts(*self.0, real_pts); }
    }
    pub fn dts(&self) -> i64 { unsafe { moonfire_ffmpeg_packet_dts(*self.0) } }
    pub fn set_dts(&mut self, dts: i64) {
        unsafe { moonfire_ffmpeg_packet_set_dts(*self.0, dts); }
    }
    pub fn duration(&self) -> i32 { unsafe { moonfire_ffmpeg_packet_duration(*self.0) } }
    pub fn set_duration(&mut self, dur: i32) {
        unsafe { moonfire_ffmpeg_packet_set_duration(*self.0, dur) }
    }
    pub fn stream_index(&self) -> usize {
        unsafe { moonfire_ffmpeg_packet_stream_index(*self.0) as usize }
    }
    pub fn data(&self) -> Option<&[u8]> {
        unsafe {
            let d = moonfire_ffmpeg_packet_data(*self.0);
            if d.data.is_null() {
                None
            } else {
                Some(::std::slice::from_raw_parts(d.data, d.len))
            }
        }
    }
}

impl<'i> Drop for Packet<'i> {
    fn drop(&mut self) {
        unsafe {
            av_packet_unref(*self.0);
        }
    }
}

pub struct Streams<'owner>(&'owner [*const AVStream]);

impl<'owner> Streams<'owner> {
    pub fn get(&self, i: usize) -> Stream<'owner> { Stream(unsafe { self.0[i].as_ref() }.unwrap()) }
    pub fn len(&self) -> usize { self.0.len() }
}

pub struct Stream<'o>(&'o AVStream);

impl<'o> Stream<'o> {
    pub fn codec<'s>(&'s self) -> CodecContext<'s> {
        CodecContext(unsafe { moonfire_ffmpeg_stream_codec(self.0).as_ref() }.unwrap())
    }

    pub fn time_base(&self) -> AVRational {
        unsafe { moonfire_ffmpeg_stream_time_base(self.0) }
    }
}

pub struct CodecContext<'s>(&'s AVCodecContext);

impl<'s> CodecContext<'s> {
    pub fn extradata(&self) -> &[u8] {
        unsafe {
            let d = moonfire_ffmpeg_cctx_extradata(self.0);
            ::std::slice::from_raw_parts(d.data, d.len)
        }
    }
    pub fn width(&self) -> libc::c_int { unsafe { moonfire_ffmpeg_cctx_width(self.0) } }
    pub fn height(&self) -> libc::c_int { unsafe { moonfire_ffmpeg_cctx_height(self.0) } }
    pub fn codec_id(&self) -> CodecId {
        CodecId(unsafe { moonfire_ffmpeg_cctx_codec_id(self.0) })
    }
    pub fn codec_type(&self) -> MediaType {
        MediaType(unsafe { moonfire_ffmpeg_cctx_codec_type(self.0) })
    }
}

#[derive(Copy, Clone, Debug)]
pub struct CodecId(libc::c_int);

impl CodecId {
    pub fn is_h264(self) -> bool { self.0 == unsafe { moonfire_ffmpeg_av_codec_id_h264 } }
}

#[derive(Copy, Clone, Debug)]
pub struct MediaType(libc::c_int);

impl MediaType {
    pub fn is_video(self) -> bool { self.0 == unsafe { moonfire_ffmpeg_avmedia_type_video } }
}

#[derive(Copy, Clone, Debug)]
pub struct Error(libc::c_int);

impl Error {
    pub fn eof() -> Self { Error(unsafe { moonfire_ffmpeg_averror_eof }) }

    fn wrap(raw: libc::c_int) -> Result<(), Error> {
        match raw {
            0 => Ok(()),
            r => Err(Error(r)),
        }
    }

    pub fn is_eof(self) -> bool { return self.0 == unsafe { moonfire_ffmpeg_averror_eof } }
}

impl std::error::Error for Error {
    fn description(&self) -> &str {
        // TODO: pull out some common cases.
        "ffmpeg error"
    }

    fn cause(&self) -> Option<&std::error::Error> { None }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        const ARRAYLEN: usize = 64;
        let mut buf = [0; ARRAYLEN];
        let s = unsafe {
            // Note av_strerror uses strlcpy, so it guarantees a trailing NUL byte.
            av_strerror(self.0, buf.as_mut_ptr(), ARRAYLEN);
            CStr::from_ptr(buf.as_ptr())
        };
        f.write_str(&s.to_string_lossy())
    }
}

#[derive(Copy, Clone)]
struct Version(libc::c_int);

impl Version {
    fn major(self) -> libc::c_int { (self.0 >> 16) & 0xFF }
    fn minor(self) -> libc::c_int { (self.0 >> 8) & 0xFF }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}.{}.{}", (self.0 >> 16) & 0xFF, (self.0 >> 8) & 0xFF, self.0 & 0xFF)
    }
}

struct Library {
    name: &'static str,
    compiled: Version,
    running: Version,
}

impl Library {
    fn new(name: &'static str, compiled: libc::c_int, running: libc::c_int) -> Self {
        Library {
            name,
            compiled: Version(compiled),
            running: Version(running),
        }
    }

    fn is_compatible(&self) -> bool {
        self.running.major() == self.compiled.major() &&
            self.running.minor() >= self.compiled.minor()
    }
}

impl fmt::Display for Library {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}: running={} compiled={}", self.name, self.running, self.compiled)
    }
}

pub struct Dictionary(*mut AVDictionary);

impl Dictionary {
    pub fn new() -> Dictionary { Dictionary(ptr::null_mut()) }
    pub fn size(&self) -> usize { (unsafe { av_dict_count(self.0) }) as usize }
    pub fn empty(&self) -> bool { self.size() == 0 }
    pub fn set(&mut self, key: &CStr, value: &CStr) -> Result<(), Error> {
        Error::wrap(unsafe { av_dict_set(&mut self.0, key.as_ptr(), value.as_ptr(), 0) })
    }
}

impl fmt::Display for Dictionary {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut ent = ptr::null_mut();
        let mut first = true;
        loop {
            unsafe {
                let c = 0;
                ent = av_dict_get(self.0, &c, ent, moonfire_ffmpeg_av_dict_ignore_suffix);
                if ent.is_null() {
                    break;
                }
                if first {
                    first = false;
                } else {
                    write!(f, ", ")?;
                }
                write!(f, "{}={}", CStr::from_ptr((*ent).key).to_string_lossy(),
                      CStr::from_ptr((*ent).value).to_string_lossy())?;
            }
        }
        Ok(())
    }
}

impl Drop for Dictionary {
    fn drop(&mut self) { unsafe { av_dict_free(&mut self.0) } }
}

impl Ffmpeg {
    pub fn new() -> Ffmpeg {
        START.call_once(|| unsafe {
            let libs = &[
                Library::new("avutil", moonfire_ffmpeg_compiled_libavutil_version,
                             avutil_version()),
                Library::new("avcodec", moonfire_ffmpeg_compiled_libavcodec_version,
                             avcodec_version()),
                Library::new("avformat", moonfire_ffmpeg_compiled_libavformat_version,
                             avformat_version()),
            ];
            let mut msg = String::new();
            let mut compatible = true;
            for l in libs {
                write!(&mut msg, "\n{}", l).unwrap();
                if !l.is_compatible() {
                    compatible = false;
                    msg.push_str(" <- not ABI-compatible!");
                }
            }
            if !compatible {
                panic!("Incompatible ffmpeg versions:{}", msg);
            }
            moonfire_ffmpeg_init();
            av_register_all();
            if avformat_network_init() < 0 {
                panic!("avformat_network_init failed");
            }
            info!("Initialized ffmpeg. Versions:{}", msg);
        });
        Ffmpeg{}
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::CString;
    use super::Error;

    /// Just tests that this doesn't crash with an ABI compatibility error.
    #[test]
    fn test_init() { super::Ffmpeg::new(); }

    #[test]
    fn test_is_compatible() {
        // compiled major/minor/patch, running major/minor/patch, expected compatible
        use ::libc::c_int;
        struct Test(c_int, c_int, c_int, c_int, c_int, c_int, bool);

        let tests = &[
            Test(55, 1, 2, 55, 1, 2, true),   // same version, compatible
            Test(55, 1, 2, 55, 2, 1, true),   // newer minor version, compatible
            Test(55, 1, 3, 55, 1, 2, true),   // older patch version, compatible (but weird)
            Test(55, 2, 2, 55, 1, 2, false),  // older minor version, incompatible
            Test(55, 1, 2, 56, 1, 2, false),  // newer major version, incompatible
            Test(56, 1, 2, 55, 1, 2, false),  // older major version, incompatible
        ];

        for t in tests {
            let l = super::Library::new(
                "avutil", (t.0 << 16) | (t.1 << 8) | t.2, (t.3 << 16) | (t.4 << 8) | t.5);
            assert!(l.is_compatible() == t.6, "{} expected={}", l, t.6);
        }
    }

    #[test]
    fn test_error() {
        let eof_formatted = format!("{}", Error::eof());
        assert!(eof_formatted.contains("End of file"), "eof is: {}", eof_formatted);

        // Errors should be round trippable to a CString. (This will fail if they contain NUL
        // bytes.)
        CString::new(eof_formatted).unwrap();
    }
}
