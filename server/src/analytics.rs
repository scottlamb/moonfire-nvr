// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2020 The Moonfire NVR Authors
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

//! Video analytics via TensorFlow Lite and an Edge TPU.
//!
//! Note this module is only compiled with `--features=analytics`. There's a stub implementation in
//! `src/main.rs` which is used otherwise.
//!
//! Currently results are only logged (rather spammily, on each frame), not persisted to the
//! database. This will change soon.
//!
//! Currently does object detection on every frame with a single hardcoded model: the 300x300
//! MobileNet SSD v2 (COCO) from https://coral.ai/models/. Eventually analytics might include:
//!
//! *   an object detection model retrained on surveillance images and/or larger input sizes
//!     for increased accuracy.
//! *   multiple invocations per image to improve resolution with current model sizes
//!     (either fixed, overlapping subsets of the image or zooming in on full-frame detections to
//!     increase confidence).
//! *   support for other hardware setups (GPUs, other brands of NPUs).
//! *   a motion detection model.
//! *   H.264/H.265 decoding on every frame but performing object detection at a minimum pts
//!     interval to cut down on expense.

use cstr::cstr;
use failure::{format_err, Error};
use ffmpeg;
use log::info;
use std::sync::Arc;

static MODEL: &[u8] = include_bytes!("edgetpu.tflite");

//static MODEL_UUID: Uuid = Uuid::from_u128(0x02054a38_62cf_42ff_9ffa_04876a2970d0_u128);

pub static MODEL_LABELS: [Option<&str>; 90] = [
    Some("person"),
    Some("bicycle"),
    Some("car"),
    Some("motorcycle"),
    Some("airplane"),
    Some("bus"),
    Some("train"),
    Some("truck"),
    Some("boat"),
    Some("traffic light"),
    Some("fire hydrant"),
    None,
    Some("stop sign"),
    Some("parking meter"),
    Some("bench"),
    Some("bird"),
    Some("cat"),
    Some("dog"),
    Some("horse"),
    Some("sheep"),
    Some("cow"),
    Some("elephant"),
    Some("bear"),
    Some("zebra"),
    Some("giraffe"),
    None,
    Some("backpack"),
    Some("umbrella"),
    None,
    None,
    Some("handbag"),
    Some("tie"),
    Some("suitcase"),
    Some("frisbee"),
    Some("skis"),
    Some("snowboard"),
    Some("sports ball"),
    Some("kite"),
    Some("baseball bat"),
    Some("baseball glove"),
    Some("skateboard"),
    Some("surfboard"),
    Some("tennis racket"),
    Some("bottle"),
    None,
    Some("wine glass"),
    Some("cup"),
    Some("fork"),
    Some("knife"),
    Some("spoon"),
    Some("bowl"),
    Some("banana"),
    Some("apple"),
    Some("sandwich"),
    Some("orange"),
    Some("broccoli"),
    Some("carrot"),
    Some("hot dog"),
    Some("pizza"),
    Some("donut"),
    Some("cake"),
    Some("chair"),
    Some("couch"),
    Some("potted plant"),
    Some("bed"),
    None,
    Some("dining table"),
    None,
    None,
    Some("toilet"),
    None,
    Some("tv"),
    Some("laptop"),
    Some("mouse"),
    Some("remote"),
    Some("keyboard"),
    Some("cell phone"),
    Some("microwave"),
    Some("oven"),
    Some("toaster"),
    Some("sink"),
    Some("refrigerator"),
    None,
    Some("book"),
    Some("clock"),
    Some("vase"),
    Some("scissors"),
    Some("teddy bear"),
    Some("hair drier"),
    Some("toothbrush"),
];

pub struct ObjectDetector {
    interpreter: parking_lot::Mutex<moonfire_tflite::Interpreter<'static>>,
    width: i32,
    height: i32,
}

impl ObjectDetector {
    pub fn new(/*db: &db::LockedDatabase*/) -> Result<Arc<Self>, Error> {
        let model = moonfire_tflite::Model::from_static(MODEL)
            .map_err(|()| format_err!("TensorFlow Lite model initialization failed"))?;
        let devices = moonfire_tflite::edgetpu::Devices::list();
        let device = devices
            .first()
            .ok_or_else(|| format_err!("No Edge TPU device available"))?;
        info!(
            "Using device {:?}/{:?} for object detection",
            device.type_(),
            device.path()
        );
        let mut builder = moonfire_tflite::Interpreter::builder();
        builder.add_owned_delegate(device.create_delegate().map_err(|()| {
            format_err!(
                "Unable to create delegate for {:?}/{:?}",
                device.type_(),
                device.path()
            )
        })?);
        let interpreter = builder
            .build(&model)
            .map_err(|()| format_err!("TensorFlow Lite initialization failed"))?;
        Ok(Arc::new(Self {
            interpreter: parking_lot::Mutex::new(interpreter),
            width: 300, // TODO
            height: 300,
        }))
    }
}

pub struct ObjectDetectorStream {
    decoder: ffmpeg::avcodec::DecodeContext,
    frame: ffmpeg::avutil::VideoFrame,
    scaler: ffmpeg::swscale::Scaler,
    scaled: ffmpeg::avutil::VideoFrame,
}

/// Copies from a RGB24 VideoFrame to a 1xHxWx3 Tensor.
fn copy(from: &ffmpeg::avutil::VideoFrame, to: &mut moonfire_tflite::Tensor) {
    let from = from.plane(0);
    let to = to.bytes_mut();
    let (w, h) = (from.width, from.height);
    let mut from_i = 0;
    let mut to_i = 0;
    for _y in 0..h {
        to[to_i..to_i + 3 * w].copy_from_slice(&from.data[from_i..from_i + 3 * w]);
        from_i += from.linesize;
        to_i += 3 * w;
    }
}

const SCORE_THRESHOLD: f32 = 0.5;

impl ObjectDetectorStream {
    pub fn new(
        par: ffmpeg::avcodec::InputCodecParameters<'_>,
        detector: &ObjectDetector,
    ) -> Result<Self, Error> {
        let mut dopt = ffmpeg::avutil::Dictionary::new();
        dopt.set(cstr!("refcounted_frames"), cstr!("0"))?;
        let decoder = par.new_decoder(&mut dopt)?;
        let scaled = ffmpeg::avutil::VideoFrame::owned(ffmpeg::avutil::ImageDimensions {
            width: detector.width,
            height: detector.height,
            pix_fmt: ffmpeg::avutil::PixelFormat::rgb24(),
        })?;
        let frame = ffmpeg::avutil::VideoFrame::empty()?;
        let scaler = ffmpeg::swscale::Scaler::new(par.dims(), scaled.dims())?;
        Ok(Self {
            decoder,
            frame,
            scaler,
            scaled,
        })
    }

    pub fn process_frame(
        &mut self,
        pkt: &ffmpeg::avcodec::Packet<'_>,
        detector: &ObjectDetector,
    ) -> Result<(), Error> {
        if !self.decoder.decode_video(pkt, &mut self.frame)? {
            return Ok(());
        }
        self.scaler.scale(&self.frame, &mut self.scaled);
        let mut interpreter = detector.interpreter.lock();
        copy(&self.scaled, &mut interpreter.inputs()[0]);
        interpreter
            .invoke()
            .map_err(|()| format_err!("TFLite interpreter invocation failed"))?;
        let outputs = interpreter.outputs();
        let classes = outputs[1].f32s();
        let scores = outputs[2].f32s();
        for (i, &score) in scores.iter().enumerate() {
            if score < SCORE_THRESHOLD {
                continue;
            }
            let class = classes[i] as usize;
            if class >= MODEL_LABELS.len() {
                continue;
            }
            let label = match MODEL_LABELS[class] {
                None => continue,
                Some(l) => l,
            };
            info!("{}, score {}", label, score);
        }
        Ok(())
    }
}
