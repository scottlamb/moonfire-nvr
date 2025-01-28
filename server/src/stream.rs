// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2016 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

use base::{bail, err, Error};
use bytes::Bytes;
use futures::StreamExt;
use retina::client::Demuxed;
use retina::codec::CodecItem;
use std::pin::Pin;
use std::result::Result;
use tracing::Instrument;
use url::Url;

static RETINA_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

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

/// Gets the pixel aspect ratio to use if none is specified.
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
    #[cfg(test)]
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
            .block_on(
                rt_handle.spawn(
                    tokio::time::timeout(
                        RETINA_TIMEOUT,
                        RetinaStreamInner::play(label, url, options),
                    )
                    .in_current_span(),
                ),
            )
            .expect("RetinaStream::play task panicked, see earlier error")
            .map_err(|e| {
                err!(
                    DeadlineExceeded,
                    msg("unable to play stream and get first frame within {RETINA_TIMEOUT:?}"),
                    source(e),
                )
            })??;
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

fn params_to_sample_entry(
    params: &retina::codec::VideoParameters,
) -> Result<db::VideoSampleEntryToInsert, Error> {
    let (width, height) = params.pixel_dimensions();
    let width = u16::try_from(width).map_err(|e| err!(Unknown, source(e)))?;
    let height = u16::try_from(height).map_err(|e| err!(Unknown, source(e)))?;
    let aspect = default_pixel_aspect_ratio(width, height);
    Ok(db::VideoSampleEntryToInsert {
        data: params
            .mp4_sample_entry()
            .with_aspect_ratio(aspect)
            .build()
            .map_err(|e| err!(Unknown, source(e)))?,
        rfc6381_codec: params.rfc6381_codec().to_owned(),
        width,
        height,
        pasp_h_spacing: aspect.0,
        pasp_v_spacing: aspect.1,
    })
}

impl RetinaStreamInner {
    /// Plays to first frame. No timeout; that's the caller's responsibility.
    async fn play(
        label: String,
        url: Url,
        options: Options,
    ) -> Result<(Box<Self>, retina::codec::VideoFrame), Error> {
        let mut session = retina::client::Session::describe(url, options.session)
            .await
            .map_err(|e| err!(Unknown, source(e)))?;
        tracing::debug!("connected to {:?}, tool {:?}", &label, session.tool());
        let video_i = session
            .streams()
            .iter()
            .position(|s| {
                s.media() == "video" && matches!(s.encoding_name(), "h264" | "h265" | "jpeg")
            })
            .ok_or_else(|| {
                err!(
                    FailedPrecondition,
                    msg("couldn't find supported video stream")
                )
            })?;
        session
            .setup(video_i, options.setup)
            .await
            .map_err(|e| err!(Unknown, source(e)))?;
        let session = session
            .play(retina::client::PlayOptions::default())
            .await
            .map_err(|e| err!(Unknown, source(e)))?;
        let mut session = session.demuxed().map_err(|e| err!(Unknown, source(e)))?;

        // First frame.
        let first_frame = loop {
            match Pin::new(&mut session).next().await {
                None => bail!(Unavailable, msg("stream closed before first frame")),
                Some(Err(e)) => bail!(Unknown, msg("unable to get first frame"), source(e)),
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
            None => bail!(Unknown, msg("couldn't find video parameters")),
        };
        let video_sample_entry = params_to_sample_entry(&video_params)?;
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
            match Pin::new(&mut self.session)
                .next()
                .await
                .transpose()
                .map_err(|e| err!(Unknown, source(e)))?
            {
                None => bail!(Unavailable, msg("end of stream")),
                Some(CodecItem::VideoFrame(v)) => {
                    if v.loss() > 0 {
                        tracing::warn!(
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
                    .block_on(
                        self.rt_handle.spawn(
                            tokio::time::timeout(RETINA_TIMEOUT, inner.fetch_next_frame())
                                .in_current_span(),
                        ),
                    )
                    .expect("fetch_next_frame task panicked, see earlier error")
                    .map_err(|e| {
                        err!(
                            DeadlineExceeded,
                            msg("unable to get next frame within {RETINA_TIMEOUT:?}"),
                            source(e)
                        )
                    })??;
                let mut new_video_sample_entry = false;
                if let Some(p) = new_parameters {
                    let video_sample_entry = params_to_sample_entry(&p)?;
                    if video_sample_entry != inner.video_sample_entry {
                        tracing::debug!(
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
                Ok::<_, Error>((frame, new_video_sample_entry))
            })?;
        Ok(VideoFrame {
            pts: frame.timestamp().elapsed(),
            #[cfg(test)]
            duration: 0,
            is_key: frame.is_random_access_point(),
            data: frame.into_data().into(),
            new_video_sample_entry,
        })
    }
}

#[cfg(test)]
pub mod testutil {
    use mp4::mp4box::WriteBox as _;

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
            let reader = mp4::Mp4Reader::read_header(
                Cursor::new(f),
                u64::try_from(len).expect("len should be in u64 range"),
            )
            .map_err(|e| err!(Unknown, source(e)))?;
            let h264_track = match reader
                .tracks()
                .values()
                .find(|t| matches!(t.media_type(), Ok(mp4::MediaType::H264)))
            {
                None => bail!(
                    InvalidArgument,
                    msg(
                        "expected a H.264 track, tracks were: {:#?}",
                        reader.tracks()
                    )
                ),
                Some(t) => t,
            };
            let mut data = Vec::new();
            h264_track
                .trak
                .mdia
                .minf
                .stbl
                .stsd
                .avc1
                .as_ref()
                .unwrap()
                .write_box(&mut data)
                .unwrap();
            let video_sample_entry = db::VideoSampleEntryToInsert {
                data,
                rfc6381_codec: "avc1.4d401e".to_string(),
                width: h264_track.width(),
                height: h264_track.height(),
                pasp_h_spacing: 1,
                pasp_v_spacing: 1,
            };
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
                .read_sample(self.h264_track_id, self.next_sample_id)
                .map_err(|e| err!(Unknown, source(e)))?
                .ok_or_else(|| err!(OutOfRange, msg("end of file")))?;
            self.next_sample_id += 1;
            Ok(VideoFrame {
                pts: sample.start_time as i64,
                #[cfg(test)]
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

#[cfg(test)]
mod tests {
    use db::testutil;

    #[test]
    fn pixel_aspect_ratios() {
        testutil::init();
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
}
