// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import React, { ReactNode } from "react";
import { Camera } from "../types";
import { Part, parsePart } from "./parser";
import * as api from "../api";
import Box from "@mui/material/Box";
import CircularProgress from "@mui/material/CircularProgress";
import Alert from "@mui/material/Alert";
import useResizeObserver from "@react-hook/resize-observer";
import { fillAspect } from "../aspect";

/// The media source API to use:
/// * Essentially everything but iPhone supports `MediaSource`.
///   (All major desktop browsers; Android browsers; and Safari on iPad are
///   fine.)
/// * Safari/macOS and Safari/iPhone on iOS 17+ support `ManagedMediaSource`.
/// * Safari/iPhone with older iOS does not support anything close to
///   `MediaSource`.
export const MediaSourceApi: typeof MediaSource | undefined =
  (self as any).ManagedMediaSource ?? self.MediaSource;

interface LiveCameraProps {
  /// Caller should provide a failure path when `MediaSourceApi` is undefined
  /// and pass it back here otherwise.
  mediaSourceApi: typeof MediaSource;
  camera: Camera | null;
  chooser: JSX.Element;
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
  message: ReactNode;
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
    mediaSourceApi: typeof MediaSource,
    camera: Camera,
    setPlaybackState: (state: PlaybackState) => void,
    setAspect: (aspect: [number, number]) => void,
    video: HTMLVideoElement
  ) {
    this.mediaSourceApi = mediaSourceApi;
    this.src = new mediaSourceApi();
    this.camera = camera;
    this.setPlaybackState = setPlaybackState;
    this.setAspect = setAspect;
    this.video = video;
    video.addEventListener("pause", this.videoPause);
    video.addEventListener("play", this.videoPlay);
    video.addEventListener("playing", this.videoPlaying);
    video.addEventListener("timeupdate", this.videoTimeUpdate);
    video.addEventListener("waiting", this.videoWaiting);
    this.src.addEventListener("sourceopen", this.onMediaSourceOpen);

    // This appears necessary for the `ManagedMediaSource` API to function
    // on Safari/iOS.
    video["disableRemotePlayback"] = true;
    video.src = this.objectUrl = URL.createObjectURL(this.src);
    video.load();
  }

  unmount = () => {
    this.stopStream("unmount");
    const v = this.video;
    v.removeEventListener("pause", this.videoPause);
    v.removeEventListener("play", this.videoPlay);
    v.removeEventListener("playing", this.videoPlaying);
    v.removeEventListener("timeupdate", this.videoTimeUpdate);
    v.removeEventListener("waiting", this.videoWaiting);
    v.src = "";
    URL.revokeObjectURL(this.objectUrl);
    v.load();
  };

  onMediaSourceOpen = () => {
    this.startStream("sourceopen");
  };

  startStream = (reason: string) => {
    if (this.ws !== undefined) {
      return;
    }
    const subStream = this.camera.streams.sub;
    if (subStream === undefined || !subStream.record) {
      this.error(
        "Must have sub stream set to record",
        <span>
          see{" "}
          <a
            href="https://github.com/scottlamb/moonfire-nvr/issues/119"
            target="_blank"
            rel="noopener noreferrer"
          >
            #119
          </a>{" "}
          and{" "}
          <a
            href="https://github.com/scottlamb/moonfire-nvr/issues/120"
            target="_blank"
            rel="noopener noreferrer"
          >
            #120
          </a>
        </span>
      );
      return;
    }
    console.log(`${this.camera.shortName}: starting stream: ${reason}`);
    const loc = window.location;
    const proto = loc.protocol === "https:" ? "wss" : "ws";

    // TODO: switch between sub and main based on window size/bandwidth.
    const url = `${proto}://${loc.host}/api/cameras/${this.camera.uuid}/sub/live.m4s`;
    this.ws = new WebSocket(url);
    this.ws.addEventListener("close", this.onWsClose);
    this.ws.addEventListener("open", this.onWsOpen);
    this.ws.addEventListener("error", this.onWsError);
    this.ws.addEventListener("message", this.onWsMessage);
  };

  error = (reason: string, extra?: ReactNode) => {
    console.error(`${this.camera.shortName}: aborting due to ${reason}`);
    this.stopStream(reason);
    this.buf = { state: "error" };
    this.setPlaybackState({
      state: "error",
      message: extra ? (
        <div>
          {reason} {extra}
        </div>
      ) : (
        reason
      ),
    });
  };

  onWsClose = (e: CloseEvent) => {
    // e doesn't say much. code is likely 1006, reason is likely empty.
    // See the warning here: https://websockets.spec.whatwg.org/#closeWebSocket
    const cleanly = e.wasClean ? "cleanly" : "uncleanly";
    this.error(`connection closed ${cleanly}`);
  };

  onWsOpen = (e: Event) => {
    console.debug(`${this.camera.shortName}: ws open`);
  };

  onWsError = (e: Event) => {
    console.error(`${this.camera.shortName}: ws error`, e);
  };

  onWsMessage = (e: MessageEvent<any>) => {
    if (typeof e.data === "string") {
      // error message.
      this.error(`server: ${e.data}`);
      return;
    }
    // Process blobs sequentially by chaining onto a promise. This prevents
    // concurrent Blob.arrayBuffer() calls from resolving out of order and
    // delivering segments to the SourceBuffer out of order.
    this.messageChain = this.messageChain.then(() =>
      this.processWsBlob(e.data as Blob)
    );
  };

  messageChain: Promise<void> = Promise.resolve();

  processWsBlob = async (blob: Blob) => {
    let raw;
    try {
      raw = new Uint8Array(await blob.arrayBuffer());
    } catch (e) {
      if (!(e instanceof DOMException)) {
        throw e;
      }
      this.error(`error reading part: ${(e as DOMException).message}`);
      return;
    }
    if (this.buf.state === "error") {
      return;
    }
    let result = parsePart(raw);
    if (result.status === "error") {
      this.error(`unparseable part: ${result.errorMessage}`);
      return;
    }
    const part = result.part;
    if (!this.mediaSourceApi.isTypeSupported(part.mimeType)) {
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
      switch (initSegmentResult.status) {
        case "error":
          this.error(`init segment fetch error: ${initSegmentResult.message}`);
          return;
        case "aborted":
          this.error(`init segment fetch aborted`);
          return;
        case "success":
          break;
      }
      this.setAspect(initSegmentResult.response.aspect);
      srcBuf.appendBuffer(initSegmentResult.response.body);
      return;
    } else if (this.buf.state === "open") {
      this.tryAppendPart(this.buf);
    }
  };

  bufUpdateEnd = () => {
    if (this.buf.state !== "open") {
      console.error(
        `${this.camera.shortName}: bufUpdateEnd in state ${this.buf.state}`
      );
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
      if (!(e instanceof DOMException)) {
        throw e;
      }
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
      this.buf.srcBuf.buffered.length === 0
    ) {
      return;
    }
    const curTs = this.video.currentTime;

    // TODO: call out key frames in the part headers. The "- 5" here is a guess
    // to avoid removing anything from the current GOP.
    const sb = this.buf.srcBuf;
    const firstTs = sb.buffered.start(0);
    if (firstTs < curTs - 5) {
      sb.remove(firstTs, curTs - 5);
      this.buf.busy = true;
    }
  };

  bufEvent = (e: Event) => {
    this.error(`SourceBuffer ${e.type}`);
  };

  videoPause = () => {
    this.stopStream("pause");
  };

  videoPlay = () => {
    this.startStream("play");
  };

  videoPlaying = () => {
    if (this.buf.state !== "error") {
      this.setPlaybackState({ state: "normal" });
    }
  };

  videoTimeUpdate = () => {};

  videoWaiting = () => {
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
    this.ws.removeEventListener("open", this.onWsOpen);
    this.ws.removeEventListener("error", this.onWsError);
    this.ws.removeEventListener("message", this.onWsMessage);
    this.ws = undefined;
  };

  camera: Camera;
  setPlaybackState: (state: PlaybackState) => void;
  setAspect: (aspect: [number, number]) => void;
  video: HTMLVideoElement;

  mediaSourceApi: typeof MediaSource;
  src: MediaSource;
  buf: BufferState = { state: "closed" };
  queue: Part[] = [];
  queuedBytes: number = 0;

  /// The object URL for the HTML video element, not the WebSocket URL.
  objectUrl: string;

  ws?: WebSocket;
}

/**
 * A live view of a camera.
 *
 * Note there's a significant setup cost to creating a LiveCamera, so the parent
 * should use React's <tt>key</tt> attribute to avoid unnecessarily mounting
 * and unmounting a camera.
 */
const LiveCamera = ({ mediaSourceApi, camera, chooser }: LiveCameraProps) => {
  const [aspect, setAspect] = React.useState<[number, number]>([16, 9]);
  const videoRef = React.useRef<HTMLVideoElement>(null);
  const boxRef = React.useRef<HTMLElement>(null);
  const [playbackState, setPlaybackState] = React.useState<PlaybackState>({
    state: "normal",
  });

  React.useLayoutEffect(() => {
    fillAspect(boxRef.current!.getBoundingClientRect(), videoRef, aspect);
  }, [boxRef, videoRef, aspect]);
  useResizeObserver(boxRef, (entry: ResizeObserverEntry) => {
    fillAspect(entry.contentRect, videoRef, aspect);
  });

  // Load the camera driver.
  React.useEffect(() => {
    setPlaybackState({ state: "normal" });
    const video = videoRef.current;
    if (camera === null || video === null) {
      return;
    }
    const d = new LiveCameraDriver(
      mediaSourceApi,
      camera,
      setPlaybackState,
      setAspect,
      video
    );
    return () => {
      d.unmount();
    };
  }, [mediaSourceApi, camera]);

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

  return (
    <Box
      ref={boxRef}
      sx={{
        width: "100%",
        height: "100%",
        position: "relative",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        "& video": {
          width: "100%",
          height: "100%",

          objectFit: "contain",
        },
        "& .controls": {
          position: "absolute",
          width: "100%",
          height: "100%",
          zIndex: 1,
        },
        "& .progress-overlay": {
          position: "absolute",
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          width: "100%",
          height: "100%",
        },
        "& .alert-overlay": {
          position: "absolute",
          display: "flex",
          width: "100%",
          height: "100%",
          alignItems: "flex-end",
          p: 1,
          zIndex: 2,
        },
      }}
    >
      <div className="controls">{chooser}</div>
      {showProgress && (
        <div className="progress-overlay">
          <CircularProgress />
        </div>
      )}
      {playbackState.state === "error" && (
        <div className="alert-overlay" style={{ pointerEvents: "none" }}>
          <Alert severity="error" style={{ pointerEvents: "auto" }}>
            {playbackState.message}
          </Alert>
        </div>
      )}
      <video ref={videoRef} muted autoPlay playsInline />
    </Box>
  );
};

export default LiveCamera;
