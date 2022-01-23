// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2016 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

use crate::h264;
use cstr::cstr;
use failure::format_err;
use failure::{bail, Error};
use futures::StreamExt;
use lazy_static::lazy_static;
use log::warn;
use retina::client::{Credentials, Transport};
use retina::codec::{CodecItem, VideoParameters};
use std::convert::TryFrom;
use std::ffi::CString;
use std::pin::Pin;
use std::result::Result;
use std::sync::Arc;
use url::Url;

static START_FFMPEG: parking_lot::Once = parking_lot::Once::new();

static RETINA_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

lazy_static! {
    pub static ref FFMPEG: Ffmpeg = Ffmpeg::new();
}

pub enum RtspLibrary {
    Ffmpeg,
    Retina,
}

impl std::str::FromStr for RtspLibrary {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "ffmpeg" => RtspLibrary::Ffmpeg,
            "retina" => RtspLibrary::Retina,
            _ => bail!("unknown RTSP library {:?}", s),
        })
    }
}

impl RtspLibrary {
    pub fn opener(&self) -> &'static dyn Opener {
        match self {
            RtspLibrary::Ffmpeg => &*FFMPEG,
            RtspLibrary::Retina => &RETINA,
        }
    }
}

#[cfg(test)]
pub enum Source<'a> {
    /// A filename, for testing.
    File(&'a str),

    /// An RTSP stream, for production use.
    Rtsp {
        url: Url,
        username: Option<String>,
        password: Option<String>,
        transport: Transport,
        session_group: Arc<retina::client::SessionGroup>,
    },
}

#[cfg(not(test))]
pub enum Source {
    /// An RTSP stream, for production use.
    Rtsp {
        url: Url,
        username: Option<String>,
        password: Option<String>,
        transport: Transport,
        session_group: Arc<retina::client::SessionGroup>,
    },
}

pub trait Opener: Send + Sync {
    fn open(&self, label: String, src: Source)
        -> Result<(h264::ExtraData, Box<dyn Stream>), Error>;
}

pub struct VideoFrame<'a> {
    pub pts: i64,

    /// An estimate of the duration of the frame, or zero.
    /// This can be deceptive and is only used by some testing code.
    pub duration: i32,

    pub is_key: bool,
    pub data: &'a [u8],
}

pub trait Stream: Send {
    fn next(&mut self) -> Result<VideoFrame, Error>;
}

pub struct Ffmpeg {}

impl Ffmpeg {
    fn new() -> Ffmpeg {
        START_FFMPEG.call_once(|| {
            ffmpeg::Ffmpeg::new();
        });
        Ffmpeg {}
    }
}

impl Opener for Ffmpeg {
    fn open(
        &self,
        label: String,
        src: Source,
    ) -> Result<(h264::ExtraData, Box<dyn Stream>), Error> {
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
                        "{}: While opening URL {}, some options were not understood: {}",
                        &label, url, open_options
                    );
                }
                i
            }
            Source::Rtsp {
                url,
                username,
                password,
                transport,
                ..
            } => {
                let mut open_options = ffmpeg::avutil::Dictionary::new();
                open_options
                    .set(
                        cstr!("rtsp_transport"),
                        match transport {
                            Transport::Tcp => cstr!("tcp"),
                            Transport::Udp => cstr!("udp"),
                        },
                    )
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

                let mut url_with_credentials = url.clone();
                if let Some(u) = username.as_deref() {
                    url_with_credentials
                        .set_username(u)
                        .map_err(|_| format_err!("unable to set username on url {}", url))?;
                }
                url_with_credentials
                    .set_password(password.as_deref())
                    .map_err(|_| format_err!("unable to set password on url {}", url))?;
                let i = InputFormatContext::open(
                    &CString::new(url_with_credentials.as_str())?,
                    &mut open_options,
                )?;
                if !open_options.empty() {
                    warn!(
                        "{}: While opening URL {}, some options were not understood: {}",
                        &label, url, open_options
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

        let video = input.streams().get(video_i);
        let codec = video.codecpar();
        let codec_id = codec.codec_id();
        if !codec_id.is_h264() {
            bail!("stream's video codec {:?} is not h264", codec_id);
        }
        let tb = video.time_base();
        if tb.num != 1 || tb.den != 90000 {
            bail!(
                "video stream has timebase {}/{}; expected 1/90000",
                tb.num,
                tb.den
            );
        }
        let dims = codec.dims();
        let extra_data = h264::ExtraData::parse(
            codec.extradata(),
            u16::try_from(dims.width)?,
            u16::try_from(dims.height)?,
        )?;
        let need_transform = extra_data.need_transform;
        let stream = Box::new(FfmpegStream {
            input,
            video_i,
            data: Vec::new(),
            need_transform,
        });
        Ok((extra_data, stream))
    }
}

struct FfmpegStream {
    input: ffmpeg::avformat::InputFormatContext<'static>,
    video_i: usize,
    data: Vec<u8>,
    need_transform: bool,
}

impl Stream for FfmpegStream {
    fn next(&mut self) -> Result<VideoFrame, Error> {
        let pkt = loop {
            let pkt = self.input.read_frame()?;
            if pkt.stream_index() == self.video_i {
                break pkt;
            }
        };
        let data = pkt
            .data()
            .ok_or_else(|| format_err!("packet with no data"))?;
        if self.need_transform {
            h264::transform_sample_data(data, &mut self.data)?;
        } else {
            // This copy isn't strictly necessary, but this path is only taken in testing anyway.
            self.data.clear();
            self.data.extend_from_slice(data);
        }
        let pts = pkt.pts().ok_or_else(|| format_err!("packet with no pts"))?;
        Ok(VideoFrame {
            pts,
            is_key: pkt.is_key(),
            duration: pkt.duration(),
            data: &self.data,
        })
    }
}

pub struct RetinaOpener {}

pub const RETINA: RetinaOpener = RetinaOpener {};

impl Opener for RetinaOpener {
    fn open(
        &self,
        label: String,
        src: Source,
    ) -> Result<(h264::ExtraData, Box<dyn Stream>), Error> {
        let (startup_tx, startup_rx) = tokio::sync::oneshot::channel();
        let (frame_tx, frame_rx) = tokio::sync::mpsc::channel(1);
        let handle = tokio::runtime::Handle::current();
        let (url, options) = match src {
            #[cfg(test)]
            Source::File(_) => bail!("Retina doesn't support .mp4 files"),
            Source::Rtsp {
                url,
                username,
                password,
                transport,
                session_group,
            } => (
                url,
                retina::client::SessionOptions::default()
                    .creds(match (username, password) {
                        (None, None) => None,
                        (Some(username), password) => Some(Credentials {
                            username,
                            password: password.unwrap_or_default(),
                        }),
                        _ => bail!("must supply username when supplying password"),
                    })
                    .transport(transport)
                    .session_group(session_group)
                    .user_agent(format!("Moonfire NVR {}", env!("CARGO_PKG_VERSION"))),
            ),
        };

        handle.spawn(async move {
            let r = tokio::time::timeout(RETINA_TIMEOUT, RetinaOpener::play(url, options)).await;
            let (mut session, video_params, first_frame) =
                match r.unwrap_or_else(|_| Err(format_err!("timeout opening stream"))) {
                    Err(e) => {
                        let _ = startup_tx.send(Err(e));
                        return;
                    }
                    Ok((s, p, f)) => (s, p, f),
                };
            if startup_tx.send(Ok(video_params)).is_err() {
                return;
            }
            if frame_tx.send(Ok(first_frame)).await.is_err() {
                return;
            }

            // Read following frames.
            let mut deadline = tokio::time::Instant::now() + RETINA_TIMEOUT;
            loop {
                match tokio::time::timeout_at(deadline, session.next()).await {
                    Err(_) => {
                        let _ = frame_tx
                            .send(Err(format_err!("timeout getting next frame")))
                            .await;
                        return;
                    }
                    Ok(Some(Err(e))) => {
                        let _ = frame_tx.send(Err(e.into())).await;
                        return;
                    }
                    Ok(None) => break,
                    Ok(Some(Ok(CodecItem::VideoFrame(v)))) => {
                        if let Some(p) = v.new_parameters {
                            // TODO: we could start a new recording without dropping the connection.
                            let _ = frame_tx.send(Err(format_err!("parameter; change: {:?}", p)));
                            return;
                        }
                        deadline = tokio::time::Instant::now() + RETINA_TIMEOUT;
                        if v.loss > 0 {
                            log::warn!(
                                "{}: lost {} RTP packets @ {}",
                                &label,
                                v.loss,
                                v.start_ctx()
                            );
                        }
                        if frame_tx.send(Ok(v)).await.is_err() {
                            return; // other end died.
                        }
                    }
                    Ok(Some(Ok(_))) => {}
                }
            }
        });
        let video_params = handle.block_on(startup_rx)??;
        let dims = video_params.pixel_dimensions();
        let extra_data = h264::ExtraData::parse(
            video_params.extra_data(),
            u16::try_from(dims.0)?,
            u16::try_from(dims.1)?,
        )?;
        let stream = Box::new(RetinaStream {
            frame_rx,
            frame: None,
        });
        Ok((extra_data, stream))
    }
}

impl RetinaOpener {
    /// Plays to first frame. No timeout; that's the caller's responsibility.
    async fn play(
        url: Url,
        options: retina::client::SessionOptions,
    ) -> Result<
        (
            Pin<Box<retina::client::Demuxed>>,
            Box<VideoParameters>,
            retina::codec::VideoFrame,
        ),
        Error,
    > {
        let mut session = retina::client::Session::describe(url, options).await?;
        let (video_i, mut video_params) = session
            .streams()
            .iter()
            .enumerate()
            .find_map(|(i, s)| {
                if s.media == "video" {
                    Some((
                        i,
                        s.parameters().and_then(|p| match p {
                            retina::codec::Parameters::Video(v) => Some(Box::new(v.clone())),
                            _ => None,
                        }),
                    ))
                } else {
                    None
                }
            })
            .ok_or_else(|| format_err!("couldn't find H.264 video stream"))?;
        session.setup(video_i).await?;
        let session = session.play(retina::client::PlayOptions::default()).await?;
        let mut session = Box::pin(session.demuxed()?);

        // First frame.
        let first_frame = loop {
            match session.next().await {
                None => bail!("stream closed before first frame"),
                Some(Err(e)) => return Err(e.into()),
                Some(Ok(CodecItem::VideoFrame(mut v))) => {
                    if let Some(v) = v.new_parameters.take() {
                        video_params = Some(v);
                    }
                    if v.is_random_access_point {
                        break v;
                    }
                }
                Some(Ok(_)) => {}
            }
        };
        Ok((
            session,
            video_params.ok_or_else(|| format_err!("couldn't find H.264 parameters"))?,
            first_frame,
        ))
    }
}

struct RetinaStream {
    frame_rx: tokio::sync::mpsc::Receiver<Result<retina::codec::VideoFrame, Error>>,
    frame: Option<retina::codec::VideoFrame>,
}

impl Stream for RetinaStream {
    fn next(&mut self) -> Result<VideoFrame, Error> {
        // TODO: use Option::insert after bumping MSRV to 1.53.
        self.frame = Some(
            self.frame_rx
                .blocking_recv()
                .ok_or_else(|| format_err!("stream ended"))??,
        );
        let frame = self.frame.as_ref().unwrap();
        Ok(VideoFrame {
            pts: frame.timestamp.elapsed(),
            duration: 0,
            is_key: frame.is_random_access_point,
            data: &frame.data()[..],
        })
    }
}
