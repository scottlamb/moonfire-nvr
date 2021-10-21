// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import React from "react";
import * as api from "../api";
import { useSnackbars } from "../snackbars";
import { Stream } from "../types";
import TableBody from "@mui/material/TableBody";
import TableCell from "@mui/material/TableCell";
import TableRow, { TableRowProps } from "@mui/material/TableRow";
import Skeleton from "@mui/material/Skeleton";
import Alert from "@mui/material/Alert";

interface Props {
  stream: Stream;
  range90k: [number, number] | null;
  split90k?: number;
  trimStartAndEnd: boolean;
  setActiveRecording: (
    recording: [Stream, api.Recording, api.VideoSampleEntry] | null
  ) => void;
  formatTime: (time90k: number) => string;
}

const frameRateFmt = new Intl.NumberFormat([], {
  maximumFractionDigits: 0,
});

const sizeFmt = new Intl.NumberFormat([], {
  maximumFractionDigits: 1,
});

interface State {
  /**
   * The range to display.
   * During loading, this can differ from the requested range.
   */
  range90k: [number, number];
  response: { status: "skeleton" } | api.FetchResult<api.RecordingsResponse>;
}

interface RowProps extends TableRowProps {
  start: React.ReactNode;
  end: React.ReactNode;
  resolution: React.ReactNode;
  fps: React.ReactNode;
  storage: React.ReactNode;
  bitrate: React.ReactNode;
}

const Row = ({
  start,
  end,
  resolution,
  fps,
  storage,
  bitrate,
  ...rest
}: RowProps) => (
  <TableRow {...rest}>
    <TableCell align="right">{start}</TableCell>
    <TableCell align="right">{end}</TableCell>
    <TableCell align="right" className="opt">
      {resolution}
    </TableCell>
    <TableCell align="right" className="opt">
      {fps}
    </TableCell>
    <TableCell align="right" className="opt">
      {storage}
    </TableCell>
    <TableCell align="right">{bitrate}</TableCell>
  </TableRow>
);

/**
 * Creates a <tt>TableHeader</tt> and <tt>TableBody</tt> with a list of videos
 * for a given <tt>stream</tt> and <tt>range90k</tt>.
 *
 * Attempts to minimize reflows while loading. It leaves the existing content
 * (either nothing or a previous range) for a while before displaying a
 * skeleton.
 *
 * The parent is responsible for creating the greater table.
 *
 * When a video is clicked, calls <tt>setActiveRecording</tt>.
 */
const VideoList = ({
  stream,
  range90k,
  split90k,
  trimStartAndEnd,
  setActiveRecording,
  formatTime,
}: Props) => {
  const snackbars = useSnackbars();
  const [state, setState] = React.useState<State | null>(null);
  React.useEffect(() => {
    const abort = new AbortController();
    const doFetch = async (
      signal: AbortSignal,
      timerId: ReturnType<typeof setTimeout>,
      range90k: [number, number]
    ) => {
      const req: api.RecordingsRequest = {
        cameraUuid: stream.camera.uuid,
        stream: stream.streamType,
        startTime90k: range90k[0],
        endTime90k: range90k[1],
        split90k,
      };
      let response = await api.recordings(req, { signal });
      if (response.status === "success") {
        // Sort recordings in descending order by start time.
        response.response.recordings.sort((a, b) => b.startId - a.startId);
      }
      clearTimeout(timerId);
      setState({ range90k, response });
    };
    if (range90k !== null) {
      const timerId = setTimeout(
        () => setState({ range90k, response: { status: "skeleton" } }),
        1000
      );
      doFetch(abort.signal, timerId, range90k);
      return () => {
        abort.abort();
        clearTimeout(timerId);
      };
    }
  }, [range90k, split90k, snackbars, stream]);

  if (state === null) {
    return null;
  }
  let body;
  if (state.response.status === "skeleton") {
    body = (
      <Row
        role="progressbar"
        start={<Skeleton />}
        end={<Skeleton />}
        resolution={<Skeleton />}
        fps={<Skeleton />}
        storage={<Skeleton />}
        bitrate={<Skeleton />}
      />
    );
  } else if (state.response.status === "error") {
    body = (
      <TableRow>
        <TableCell colSpan={6}>
          <Alert severity="error">{state.response.message}</Alert>
        </TableCell>
      </TableRow>
    );
  } else if (state.response.status === "success") {
    const resp = state.response.response;
    body = resp.recordings.map((r: api.Recording) => {
      const vse = resp.videoSampleEntries[r.videoSampleEntryId];
      const durationSec = (r.endTime90k - r.startTime90k) / 90000;
      const rate = (r.sampleFileBytes / durationSec) * 0.000008;
      const start = trimStartAndEnd
        ? Math.max(r.startTime90k, state.range90k[0])
        : r.startTime90k;
      const end = trimStartAndEnd
        ? Math.min(r.endTime90k, state.range90k[1])
        : r.endTime90k;
      return (
        <Row
          key={r.startId}
          className="recording"
          onClick={() => setActiveRecording([stream, r, vse])}
          start={formatTime(start)}
          end={formatTime(end)}
          resolution={`${vse.width}x${vse.height}`}
          fps={frameRateFmt.format(r.videoSamples / durationSec)}
          storage={`${sizeFmt.format(r.sampleFileBytes / 1048576)} MiB`}
          bitrate={`${sizeFmt.format(rate)} Mbps`}
        />
      );
    });
  }
  return (
    <TableBody>
      <TableRow>
        <TableCell colSpan={6} className="streamHeader">
          {stream.camera.shortName} {stream.streamType}
        </TableCell>
      </TableRow>
      <Row
        start="start"
        end="end"
        resolution="resolution"
        fps="fps"
        storage="storage"
        bitrate="bitrate"
      />
      {body}
    </TableBody>
  );
};

export default VideoList;
