// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2016 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

use crate::h264;
use bytes::Bytes;
use failure::format_err;
use failure::{bail, Error};
use futures::StreamExt;
use retina::client::Demuxed;
use retina::codec::CodecItem;
use std::pin::Pin;
use std::result::Result;
use url::Url;

static RETINA_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

pub struct Options {
    pub session: retina::client::SessionOptions,
    pub setup: retina::client::SetupOptions,
}

/// Opens a RTSP stream. This is a trait for test injection.
pub trait Opener: Send + Sync {
    /// Opens the given RTSP URL.
    ///
    /// Note: despite the blocking interface, this expects to be called from
    /// the context of a multithreaded tokio runtime with IO and time enabled.
    fn open(&self, label: String, url: Url, options: Options) -> Result<Box<dyn Stream>, Error>;
}

pub struct VideoFrame {
    pub pts: i64,

    /// An estimate of the duration of the frame, or zero.
    /// This can be deceptive and is only used by some testing code.
    pub duration: i32,

    pub is_key: bool,
    pub data: Bytes,

    pub new_video_sample_entry: bool,
}

pub trait Stream: Send {
    fn tool(&self) -> Option<&retina::client::Tool>;
    fn video_sample_entry(&self) -> &db::VideoSampleEntryToInsert;
    fn next(&mut self) -> Result<VideoFrame, Error>;
}

pub struct RealOpener;

pub const OPENER: RealOpener = RealOpener;

impl Opener for RealOpener {
    fn open(
        &self,
        label: String,
        url: Url,
        mut options: Options,
    ) -> Result<Box<dyn Stream>, Error> {
        options.session = options
            .session
            .user_agent(format!("Moonfire NVR {}", env!("CARGO_PKG_VERSION")));
        let rt_handle = tokio::runtime::Handle::current();
        let (inner, first_frame) = rt_handle
            .block_on(rt_handle.spawn(tokio::time::timeout(
                RETINA_TIMEOUT,
                RetinaStreamInner::play(label, url, options),
            )))
            .expect("RetinaStream::play task panicked, see earlier error")??;
        Ok(Box::new(RetinaStream {
            inner: Some(inner),
            rt_handle,
            first_frame: Some(first_frame),
        }))
    }
}

/// Real stream, implemented with the Retina library.
///
/// Retina is asynchronous and tokio-based where currently Moonfire expects
/// a synchronous stream interface. This blocks on the tokio operations.
///
/// Experimentally, it appears faster to have one thread hand-off per frame via
/// `handle.block_on(handle.spawn(...))` rather than the same without the
/// `handle.spawn(...)`. See
/// [#206](https://github.com/scottlamb/moonfire-nvr/issues/206).
struct RetinaStream {
    /// The actual stream details used from within the tokio reactor.
    ///
    /// Spawned tokio tasks must be `'static`, so ownership is passed to the
    /// task, and then returned when it completes.
    inner: Option<Box<RetinaStreamInner>>,

    rt_handle: tokio::runtime::Handle,

    /// The first frame, if not yet returned from `next`.
    ///
    /// This frame is special because we sometimes need to fetch it as part of getting the video
    /// parameters.
    first_frame: Option<retina::codec::VideoFrame>,
}

struct RetinaStreamInner {
    label: String,
    session: Demuxed,
    video_sample_entry: db::VideoSampleEntryToInsert,
}

impl RetinaStreamInner {
    /// Plays to first frame. No timeout; that's the caller's responsibility.
    async fn play(
        label: String,
        url: Url,
        options: Options,
    ) -> Result<(Box<Self>, retina::codec::VideoFrame), Error> {
        let mut session = retina::client::Session::describe(url, options.session).await?;
        log::debug!("connected to {:?}, tool {:?}", &label, session.tool());
        let video_i = session
            .streams()
            .iter()
            .position(|s| s.media() == "video" && s.encoding_name() == "h264")
            .ok_or_else(|| format_err!("couldn't find H.264 video stream"))?;
        session.setup(video_i, options.setup).await?;
        let session = session.play(retina::client::PlayOptions::default()).await?;
        let mut session = session.demuxed()?;

        // First frame.
        let first_frame = loop {
            match Pin::new(&mut session).next().await {
                None => bail!("stream closed before first frame"),
                Some(Err(e)) => return Err(e.into()),
                Some(Ok(CodecItem::VideoFrame(v))) => {
                    if v.is_random_access_point() {
                        break v;
                    }
                }
                Some(Ok(_)) => {}
            }
        };
        let video_params = match session.streams()[video_i].parameters() {
            Some(retina::codec::ParametersRef::Video(v)) => v.clone(),
            Some(_) => unreachable!(),
            None => bail!("couldn't find H.264 parameters"),
        };
        let video_sample_entry = h264::parse_extra_data(video_params.extra_data())?;
        let self_ = Box::new(Self {
            label,
            session,
            video_sample_entry,
        });
        Ok((self_, first_frame))
    }

    /// Fetches a non-initial frame.
    async fn fetch_next_frame(
        mut self: Box<Self>,
    ) -> Result<
        (
            Box<Self>,
            retina::codec::VideoFrame,
            Option<retina::codec::VideoParameters>,
        ),
        Error,
    > {
        loop {
            match Pin::new(&mut self.session).next().await.transpose()? {
                None => bail!("end of stream"),
                Some(CodecItem::VideoFrame(v)) => {
                    if v.loss() > 0 {
                        log::warn!(
                            "{}: lost {} RTP packets @ {}",
                            &self.label,
                            v.loss(),
                            v.start_ctx()
                        );
                    }
                    let p = if v.has_new_parameters() {
                        Some(match self.session.streams()[v.stream_id()].parameters() {
                            Some(retina::codec::ParametersRef::Video(v)) => v.clone(),
                            _ => unreachable!(),
                        })
                    } else {
                        None
                    };
                    return Ok((self, v, p));
                }
                Some(_) => {}
            }
        }
    }
}

impl Stream for RetinaStream {
    fn tool(&self) -> Option<&retina::client::Tool> {
        self.inner.as_ref().unwrap().session.tool()
    }

    fn video_sample_entry(&self) -> &db::VideoSampleEntryToInsert {
        &self.inner.as_ref().unwrap().video_sample_entry
    }

    fn next(&mut self) -> Result<VideoFrame, Error> {
        let (frame, new_video_sample_entry) = self
            .first_frame
            .take()
            .map(|f| Ok((f, false)))
            .unwrap_or_else(move || {
                let inner = self.inner.take().unwrap();
                let (mut inner, frame, new_parameters) = self
                    .rt_handle
                    .block_on(self.rt_handle.spawn(tokio::time::timeout(
                        RETINA_TIMEOUT,
                        inner.fetch_next_frame(),
                    )))
                    .expect("fetch_next_frame task panicked, see earlier error")
                    .map_err(|_| format_err!("timeout getting next frame"))??;
                let mut new_video_sample_entry = false;
                if let Some(p) = new_parameters {
                    let video_sample_entry = h264::parse_extra_data(p.extra_data())?;
                    if video_sample_entry != inner.video_sample_entry {
                        log::debug!(
                            "{}: parameter change:\nold: {:?}\nnew: {:?}",
                            &inner.label,
                            &inner.video_sample_entry,
                            &video_sample_entry
                        );
                        inner.video_sample_entry = video_sample_entry;
                        new_video_sample_entry = true;
                    }
                };
                self.inner = Some(inner);
                Ok::<_, failure::Error>((frame, new_video_sample_entry))
            })?;
        Ok(VideoFrame {
            pts: frame.timestamp().elapsed(),
            duration: 0,
            is_key: frame.is_random_access_point(),
            data: frame.into_data().into(),
            new_video_sample_entry,
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
        video_sample_entry: db::VideoSampleEntryToInsert,
    }

    impl Mp4Stream {
        /// Opens a stream, with a return matching that expected by [`Opener`].
        pub fn open(path: &str) -> Result<Self, Error> {
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
            let video_sample_entry = h264::parse_extra_data(&h264_track.extra_data()?[..])?;
            let h264_track_id = h264_track.track_id();
            let stream = Mp4Stream {
                reader,
                h264_track_id,
                next_sample_id: 1,
                video_sample_entry,
            };
            Ok(stream)
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
                new_video_sample_entry: false,
            })
        }

        fn video_sample_entry(&self) -> &db::VideoSampleEntryToInsert {
            &self.video_sample_entry
        }
    }
}
