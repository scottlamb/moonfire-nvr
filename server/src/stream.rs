// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2016 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

use crate::h264;
use bytes::Bytes;
use failure::format_err;
use failure::{bail, Error};
use futures::StreamExt;
use retina::client::Demuxed;
use retina::codec::{CodecItem, VideoParameters};
use std::pin::Pin;
use std::result::Result;
use url::Url;

static RETINA_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Opens a RTSP stream. This is a trait for test injection.
pub trait Opener: Send + Sync {
    /// Opens the given RTSP URL.
    ///
    /// Note: despite the blocking interface, this expects to be called from
    /// a tokio runtime with IO and time enabled. Takes the
    /// [`tokio::runtime::Runtime`] rather than using
    /// `tokio::runtime::Handle::current()` because `Runtime::block_on` can
    /// drive IO and timers while `Handle::block_on` can not.
    fn open<'a>(
        &self,
        rt: &'a tokio::runtime::Runtime,
        label: String,
        url: Url,
        options: retina::client::SessionOptions,
    ) -> Result<(db::VideoSampleEntryToInsert, Box<dyn Stream + 'a>), Error>;
}

pub struct VideoFrame {
    pub pts: i64,

    /// An estimate of the duration of the frame, or zero.
    /// This can be deceptive and is only used by some testing code.
    pub duration: i32,

    pub is_key: bool,
    pub data: Bytes,
}

pub trait Stream: Send {
    fn tool(&self) -> Option<&retina::client::Tool>;
    fn next(&mut self) -> Result<VideoFrame, Error>;
}

pub struct RealOpener;

pub const OPENER: RealOpener = RealOpener;

impl Opener for RealOpener {
    fn open<'a>(
        &self,
        rt: &'a tokio::runtime::Runtime,
        label: String,
        url: Url,
        options: retina::client::SessionOptions,
    ) -> Result<(db::VideoSampleEntryToInsert, Box<dyn Stream + 'a>), Error> {
        let options = options.user_agent(format!("Moonfire NVR {}", env!("CARGO_PKG_VERSION")));
        let (session, video_params, first_frame) = rt.block_on(tokio::time::timeout(
            RETINA_TIMEOUT,
            RetinaStream::play(&label, url, options),
        ))??;
        let extra_data = h264::parse_extra_data(video_params.extra_data())?;
        let stream = Box::new(RetinaStream {
            rt,
            label,
            session,
            first_frame: Some(first_frame),
        });
        Ok((extra_data, stream))
    }
}

struct RetinaStream<'a> {
    rt: &'a tokio::runtime::Runtime,
    label: String,
    session: Pin<Box<Demuxed>>,

    /// The first frame, if not yet returned from `next`.
    ///
    /// This frame is special because we sometimes need to fetch it as part of getting the video
    /// parameters.
    first_frame: Option<retina::codec::VideoFrame>,
}

impl<'a> RetinaStream<'a> {
    /// Plays to first frame. No timeout; that's the caller's responsibility.
    async fn play(
        label: &str,
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
        log::debug!("connected to {:?}, tool {:?}", label, session.tool());
        let (video_i, mut video_params) = session
            .streams()
            .iter()
            .enumerate()
            .find_map(|(i, s)| {
                if s.media == "video" && s.encoding_name == "h264" {
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

    /// Fetches a non-initial frame.
    async fn fetch_next_frame(
        label: &str,
        mut session: Pin<&mut Demuxed>,
    ) -> Result<retina::codec::VideoFrame, Error> {
        loop {
            match session.next().await.transpose()? {
                None => bail!("end of stream"),
                Some(CodecItem::VideoFrame(v)) => {
                    if let Some(p) = v.new_parameters {
                        // TODO: we could start a new recording without dropping the connection.
                        bail!("parameter change: {:?}", p);
                    }
                    if v.loss > 0 {
                        log::warn!(
                            "{}: lost {} RTP packets @ {}",
                            &label,
                            v.loss,
                            v.start_ctx()
                        );
                    }
                    return Ok(v);
                }
                Some(_) => {}
            }
        }
    }
}

impl<'a> Stream for RetinaStream<'a> {
    fn tool(&self) -> Option<&retina::client::Tool> {
        Pin::into_inner(self.session.as_ref()).tool()
    }

    fn next(&mut self) -> Result<VideoFrame, Error> {
        let frame = self.first_frame.take().map(Ok).unwrap_or_else(|| {
            self.rt
                .block_on(tokio::time::timeout(
                    RETINA_TIMEOUT,
                    RetinaStream::fetch_next_frame(&self.label, self.session.as_mut()),
                ))
                .map_err(|_| format_err!("timeout getting next frame"))?
        })?;
        Ok(VideoFrame {
            pts: frame.timestamp.elapsed(),
            duration: 0,
            is_key: frame.is_random_access_point,
            data: frame.into_data(),
        })
    }
}

#[cfg(test)]
pub mod testutil {
    use super::*;
    use std::convert::TryFrom;
    use std::io::Cursor;

    pub struct Mp4Stream {
        reader: mp4::Mp4Reader<Cursor<Vec<u8>>>,
        h264_track_id: u32,
        next_sample_id: u32,
    }

    impl Mp4Stream {
        /// Opens a stream, with a return matching that expected by [`Opener`].
        pub fn open(path: &str) -> Result<(db::VideoSampleEntryToInsert, Self), Error> {
            let f = std::fs::read(path)?;
            let len = f.len();
            let reader = mp4::Mp4Reader::read_header(Cursor::new(f), u64::try_from(len)?)?;
            let h264_track = match reader
                .tracks()
                .values()
                .find(|t| matches!(t.media_type(), Ok(mp4::MediaType::H264)))
            {
                None => bail!("expected a H.264 track"),
                Some(t) => t,
            };
            let extra_data = h264::parse_extra_data(&h264_track.extra_data()?[..])?;
            let h264_track_id = h264_track.track_id();
            let stream = Mp4Stream {
                reader,
                h264_track_id,
                next_sample_id: 1,
            };
            Ok((extra_data, stream))
        }

        pub fn duration(&self) -> u64 {
            self.reader.moov.mvhd.duration
        }

        /// Returns the edit list from the H.264 stream, if any.
        pub fn elst(&self) -> Option<&mp4::mp4box::elst::ElstBox> {
            let h264_track = self.reader.tracks().get(&self.h264_track_id).unwrap();
            h264_track
                .trak
                .edts
                .as_ref()
                .and_then(|edts| edts.elst.as_ref())
        }
    }

    impl Stream for Mp4Stream {
        fn tool(&self) -> Option<&retina::client::Tool> {
            None
        }

        fn next(&mut self) -> Result<VideoFrame, Error> {
            let sample = self
                .reader
                .read_sample(self.h264_track_id, self.next_sample_id)?
                .ok_or_else(|| format_err!("End of file"))?;
            self.next_sample_id += 1;
            Ok(VideoFrame {
                pts: sample.start_time as i64,
                duration: sample.duration as i32,
                is_key: sample.is_sync,
                data: sample.bytes,
            })
        }
    }
}
