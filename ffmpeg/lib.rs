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

extern crate libc;
#[macro_use] extern crate log;

use std::cell::{Ref, RefCell};
use std::ffi::CStr;
use std::fmt;
use std::marker::PhantomData;
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
    fn av_strerror(e: libc::c_int, b: *mut u8, s: libc::size_t) -> libc::c_int;
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
        Ok(Packet { _ctx: PhantomData, pkt: pkt })
    }

    pub fn streams<'i>(&'i self) -> Streams<'i> {
        Streams {
            _owner: PhantomData,
            streams: unsafe { moonfire_ffmpeg_fctx_streams(self.ctx) },
        }
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

pub struct Packet<'i> {
    _ctx: PhantomData<&'i InputFormatContext>,
    pkt: Ref<'i, *mut AVPacket>,
}

impl<'i> Packet<'i> {
    pub fn is_key(&self) -> bool { unsafe { moonfire_ffmpeg_packet_is_key(*self.pkt) } }
    pub fn pts(&self) -> Option<i64> {
        match unsafe { moonfire_ffmpeg_packet_pts(*self.pkt) } {
            v if v == unsafe { moonfire_ffmpeg_av_nopts_value } => None,
            v => Some(v),
        }
    }
    pub fn set_pts(&mut self, pts: Option<i64>) {
        let real_pts = match pts {
            None => unsafe { moonfire_ffmpeg_av_nopts_value },
            Some(v) => v,
        };
        unsafe { moonfire_ffmpeg_packet_set_pts(*self.pkt, real_pts); }
    }
    pub fn dts(&self) -> i64 { unsafe { moonfire_ffmpeg_packet_dts(*self.pkt) } }
    pub fn set_dts(&mut self, dts: i64) {
        unsafe { moonfire_ffmpeg_packet_set_dts(*self.pkt, dts); }
    }
    pub fn duration(&self) -> i32 { unsafe { moonfire_ffmpeg_packet_duration(*self.pkt) } }
    pub fn set_duration(&mut self, dur: i32) {
        unsafe { moonfire_ffmpeg_packet_set_duration(*self.pkt, dur) }
    }
    pub fn stream_index(&self) -> usize {
        unsafe { moonfire_ffmpeg_packet_stream_index(*self.pkt) as usize }
    }
    pub fn data(&self) -> Option<&[u8]> {
        unsafe {
            let d = moonfire_ffmpeg_packet_data(*self.pkt);
            if d.data.is_null() {
                None
            } else {
                Some(::std::slice::from_raw_parts(d.data, d.len))
            }
        }
    }

    //pub fn deref(self) -> &'i InputFormatContext { self.ctx }
}

impl<'i> Drop for Packet<'i> {
    fn drop(&mut self) {
        unsafe {
            av_packet_unref(*self.pkt);
        }
    }
}

pub struct Streams<'owner> {
    _owner: PhantomData<&'owner ()>,
    streams: StreamsLen,
}

impl<'owner> Streams<'owner> {
    pub fn get(&self, i: usize) -> Stream<'owner> {
        assert!(i < self.streams.len);
        Stream {
            _owner: PhantomData,
            stream: unsafe { *self.streams.streams.offset(i as isize) }
        }
    }

    pub fn len(&self) -> usize { self.streams.len }
}

pub struct Stream<'o> {
    _owner: PhantomData<&'o ()>,
    stream: *const AVStream,
}

impl<'o> Stream<'o> {
    pub fn codec<'s>(&'s self) -> CodecContext<'s> {
        CodecContext {
            _owner: PhantomData,
            ctx: unsafe { moonfire_ffmpeg_stream_codec(self.stream) },
        }
    }

    pub fn time_base(&self) -> AVRational {
        unsafe { moonfire_ffmpeg_stream_time_base(self.stream) }
    }
}

pub struct CodecContext<'s> {
    _owner: PhantomData<&'s ()>,
    ctx: *const AVCodecContext,
}

impl<'s> CodecContext<'s> {
    pub fn extradata(&self) -> &[u8] {
        unsafe {
            let d = moonfire_ffmpeg_cctx_extradata(self.ctx);
            ::std::slice::from_raw_parts(d.data, d.len)
        }
    }
    pub fn width(&self) -> libc::c_int { unsafe { moonfire_ffmpeg_cctx_width(self.ctx) } }
    pub fn height(&self) -> libc::c_int { unsafe { moonfire_ffmpeg_cctx_height(self.ctx) } }
    pub fn codec_id(&self) -> CodecId {
        CodecId(unsafe { moonfire_ffmpeg_cctx_codec_id(self.ctx) })
    }
    pub fn codec_type(&self) -> MediaType {
        MediaType(unsafe { moonfire_ffmpeg_cctx_codec_type(self.ctx) })
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
        let mut buf = [0u8; ARRAYLEN];
        unsafe { av_strerror(self.0, buf.as_mut_ptr(), ARRAYLEN) };
        f.write_str(std::str::from_utf8(&buf).map_err(|_| fmt::Error)?)
    }
}

struct Version(libc::c_int);

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}.{}.{}", (self.0 >> 16) & 0xFF, (self.0 >> 8) & 0xFF, self.0 & 0xFF)
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
            moonfire_ffmpeg_init();
            av_register_all();
            if avformat_network_init() < 0 {
                panic!("avformat_network_init failed");
            }
            info!("Initialized ffmpeg. Versions:\n\
                  avcodec compiled={} running={}\n\
                  avformat compiled={} running={}\n\
                  avutil compiled={} running={}",
                  Version(moonfire_ffmpeg_compiled_libavcodec_version),
                  Version(avcodec_version()),
                  Version(moonfire_ffmpeg_compiled_libavformat_version),
                  Version(avformat_version()),
                  Version(moonfire_ffmpeg_compiled_libavutil_version),
                  Version(avutil_version()));
        });
        Ffmpeg{}
    }
}
