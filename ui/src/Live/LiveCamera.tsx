// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import React, { SyntheticEvent } from "react";
import { Camera } from "../types";
import { Part, parsePart } from "./parser";
import * as api from "../api";
import Box from "@material-ui/core/Box";
import CircularProgress from "@material-ui/core/CircularProgress";
import Alert from "@material-ui/core/Alert";

interface LiveCameraProps {
  camera: Camera;
}

interface BufferStateClosed {
  state: "closed";
}

interface BufferStateOpen {
  state: "open";
  srcBuf: SourceBuffer;
  busy: boolean;
  mimeType: string;
  videoSampleEntryId: number;
}

interface BufferStateError {
  state: "error";
}

type BufferState = BufferStateClosed | BufferStateOpen | BufferStateError;

interface PlaybackStateNormal {
  state: "normal";
}

interface PlaybackStateWaiting {
  state: "waiting";
}

interface PlaybackStateError {
  state: "error";
  message: string;
}

type PlaybackState =
  | PlaybackStateNormal
  | PlaybackStateWaiting
  | PlaybackStateError;

/**
 * Drives a live camera.
 * Implementation detail of LiveCamera which listens to various DOM events and
 * drives the WebSocket feed and the MediaSource and SourceBuffers.
 */
class LiveCameraDriver {
  constructor(
    camera: Camera,
    setPlaybackState: (state: PlaybackState) => void,
    videoRef: React.RefObject<HTMLVideoElement>
  ) {
    this.camera = camera;
    this.setPlaybackState = setPlaybackState;
    this.videoRef = videoRef;
    this.src.addEventListener("sourceopen", this.onMediaSourceOpen);
  }

  onMediaSourceOpen = () => {
    this.startStream("sourceopen");
  };

  startStream = (reason: string) => {
    if (this.ws !== undefined) {
      return;
    }
    console.log(`${this.camera.shortName}: starting stream: ${reason}`);
    const loc = window.location;
    const proto = loc.protocol === "https:" ? "wss" : "ws";

    // TODO: switch between sub and main based on window size/bandwidth.
    const url = `${proto}://${loc.host}/api/cameras/${this.camera.uuid}/sub/live.m4s`;
    this.ws = new WebSocket(url);
    this.ws.addEventListener("close", this.onWsClose);
    this.ws.addEventListener("error", this.onWsError);
    this.ws.addEventListener("message", this.onWsMessage);
  };

  error = (reason: string) => {
    console.error(`${this.camera.shortName}: aborting due to ${reason}`);
    this.stopStream(reason);
    this.buf = { state: "error" };
    this.src.endOfStream("network");
    this.setPlaybackState({ state: "error", message: reason });
  };

  onWsClose = (e: CloseEvent) => {
    this.error(`ws close: ${e.code} ${e.reason}`);
  };

  onWsError = (_e: Event) => {
    this.error("ws error");
  };

  onWsMessage = async (e: MessageEvent) => {
    let raw;
    try {
      raw = new Uint8Array(await e.data.arrayBuffer());
    } catch (e) {
      this.error(`error reading part: ${e.message}`);
      return;
    }
    if (this.buf.state === "error") {
      console.log("onWsMessage while in state error");
      return;
    }
    let result = parsePart(raw);
    if (result.status === "error") {
      this.error("unparseable part");
      return;
    }
    const part = result.part;
    if (!MediaSource.isTypeSupported(part.mimeType)) {
      this.error(`unsupported mime type ${part.mimeType}`);
      return;
    }

    this.queue.push(part);
    this.queuedBytes += part.body.byteLength;
    if (this.buf.state === "closed") {
      const srcBuf = this.src.addSourceBuffer(part.mimeType);
      srcBuf.mode = "segments";
      srcBuf.addEventListener("updateend", this.bufUpdateEnd);
      srcBuf.addEventListener("error", this.bufEvent);
      srcBuf.addEventListener("abort", this.bufEvent);
      this.buf = {
        state: "open",
        srcBuf,
        busy: true,
        mimeType: part.mimeType,
        videoSampleEntryId: part.videoSampleEntryId,
      };
      let initSegmentResult = await api.init(part.videoSampleEntryId, {});
      if (initSegmentResult.status !== "success") {
        this.error(`init segment fetch status ${initSegmentResult.status}`);
        return;
      }
      srcBuf.appendBuffer(initSegmentResult.response);
      return;
    } else if (this.buf.state === "open") {
      this.tryAppendPart(this.buf);
    }
  };

  bufUpdateEnd = () => {
    if (this.buf.state !== "open") {
      console.error("bufUpdateEnd in state", this.buf.state);
      return;
    }
    if (!this.buf.busy) {
      this.error("bufUpdateEnd when not busy");
      return;
    }
    this.buf.busy = false;
    this.tryTrimBuffer();
    this.tryAppendPart(this.buf);
  };

  tryAppendPart = (buf: BufferStateOpen) => {
    if (buf.busy) {
      return;
    }

    const part = this.queue.shift();
    if (part === undefined) {
      return;
    }
    this.queuedBytes -= part.body.byteLength;

    if (
      part.mimeType !== buf.mimeType ||
      part.videoSampleEntryId !== buf.videoSampleEntryId
    ) {
      this.error("Switching MIME type or videoSampleEntryId unimplemented");
      return;
    }

    // Always put the new part at the end. SourceBuffer.mode "sequence" is
    // supposed to generate timestamps automatically, but on Chrome 89.0.4389.90
    // it doesn't appear to work as expected. So use SourceBuffer.mode
    // "segments" and use the existing end as the timestampOffset.
    const b = buf.srcBuf.buffered;
    buf.srcBuf.timestampOffset = b.length > 0 ? b.end(b.length - 1) : 0;

    try {
      buf.srcBuf.appendBuffer(part.body);
    } catch (e) {
      // In particular, appendBuffer can throw QuotaExceededError.
      // <https://developers.google.com/web/updates/2017/10/quotaexceedederror>
      // tryTrimBuffer removes already-played stuff from the buffer to avoid
      // this, but in theory even one GOP could be more than the total buffer
      // size. At least report error properly.
      this.error(`${e.name} while appending buffer`);
      return;
    }
    buf.busy = true;
  };

  tryTrimBuffer = () => {
    if (
      this.buf.state !== "open" ||
      this.buf.busy ||
      this.buf.srcBuf.buffered.length === 0 ||
      this.videoRef.current === null
    ) {
      return;
    }
    const curTs = this.videoRef.current.currentTime;

    // TODO: call out key frames in the part headers. The "- 5" here is a guess
    // to avoid removing anything from the current GOP.
    const firstTs = this.buf.srcBuf.buffered.start(0);
    if (firstTs < curTs - 5) {
      console.log(`${this.camera.shortName}: trimming ${firstTs}-${curTs}`);
      this.buf.srcBuf.remove(firstTs, curTs - 5);
      this.buf.busy = true;
    }
  };

  bufEvent = (e: Event) => {
    this.error(`bufEvent: ${e}`);
  };

  videoPlaying = (e: SyntheticEvent<HTMLVideoElement, Event>) => {
    if (this.buf.state !== "error") {
      this.setPlaybackState({ state: "normal" });
    }
  };

  videoWaiting = (e: SyntheticEvent<HTMLVideoElement, Event>) => {
    if (this.buf.state !== "error") {
      this.setPlaybackState({ state: "waiting" });
    }
  };

  stopStream = (reason: string) => {
    if (this.ws === undefined) {
      return;
    }
    console.log(`${this.camera.shortName}: stopping stream: ${reason}`);
    const NORMAL_CLOSURE = 1000; // https://developer.mozilla.org/en-US/docs/Web/API/CloseEvent
    this.ws.close(NORMAL_CLOSURE);
    this.ws.removeEventListener("close", this.onWsClose);
    this.ws.removeEventListener("error", this.onWsError);
    this.ws.removeEventListener("message", this.onWsMessage);
    this.ws = undefined;
  };

  camera: Camera;
  setPlaybackState: (state: PlaybackState) => void;
  videoRef: React.RefObject<HTMLVideoElement>;

  src = new MediaSource();
  buf: BufferState = { state: "closed" };
  queue: Part[] = [];
  queuedBytes: number = 0;

  /// The object URL for the HTML video element, not the WebSocket URL.
  url = URL.createObjectURL(this.src);

  ws?: WebSocket;
}

/**
 * A live view of a camera.
 * Note there's a significant setup cost to creating a LiveCamera, so the parent
 * should use React's <tt>key</tt> attribute to avoid unnecessarily mounting
 * and unmounting a camera.
 */
const LiveCamera = ({ camera }: LiveCameraProps) => {
  const videoRef = React.useRef<HTMLVideoElement>(null);
  const [playbackState, setPlaybackState] = React.useState<PlaybackState>({
    state: "normal",
  });

  // Load the camera driver.
  const [driver, setDriver] = React.useState<LiveCameraDriver | null>(null);
  React.useEffect(() => {
    const d = new LiveCameraDriver(camera, setPlaybackState, videoRef);
    setDriver(d);
    return () => {
      // Explictly stop the stream on unmount. There don't seem to be any DOM
      // event handlers that run in this case. (In particular, the MediaSource's
      // sourceclose doesn't run.)
      d.stopStream("unmount or camera change");
    };
  }, [camera]);

  // Display circular progress after 100 ms of waiting.
  const [showProgress, setShowProgress] = React.useState(false);
  React.useEffect(() => {
    setShowProgress(false);
    if (playbackState.state !== "waiting") {
      return;
    }
    const timerId = setTimeout(() => setShowProgress(true), 100);
    return () => clearTimeout(timerId);
  }, [playbackState]);

  if (driver === null) {
    return <Box />;
  }
  return (
    <Box
      sx={{
        "& video": { width: "100%", height: "100%", objectFit: "contain" },
        "& .progress-overlay": {
          position: "absolute",
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          height: "100%",
          width: "100%",
          zIndex: 1,
        },
        "& .alert-overlay": {
          position: "absolute",
          display: "flex",
          height: "100%",
          width: "100%",
          alignItems: "flex-end",
          zIndex: 1,
          p: 1,
        },
      }}
    >
      {showProgress && (
        <div className="progress-overlay">
          <CircularProgress />
        </div>
      )}
      {playbackState.state === "error" && (
        <div className="alert-overlay">
          <Alert severity="error">{playbackState.message}</Alert>
        </div>
      )}
      <video
        ref={videoRef}
        muted
        autoPlay
        src={driver.url}
        onPause={() => driver.stopStream("pause")}
        onPlay={() => driver.startStream("play")}
        onPlaying={driver.videoPlaying}
        onTimeUpdate={driver.tryTrimBuffer}
        onWaiting={driver.videoWaiting}
      />
    </Box>
  );
};

export default LiveCamera;
