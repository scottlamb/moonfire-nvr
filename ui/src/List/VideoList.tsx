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
import Tooltip from "@mui/material/Tooltip";
import Typography from "@mui/material/Typography";

interface Props {
  stream: Stream;
  range90k: [number, number] | null;
  split90k?: number;
  trimStartAndEnd: boolean;
  setActiveRecording: (recording: [Stream, CombinedRecording] | null) => void;
  formatTime: (time90k: number) => string;
}

/**
 * Matches `api.Recording`, except that two entries with differing
 * `videoSampleEntryId` but the same resolution may be combined.
 */
export interface CombinedRecording {
  startId: number;
  endId?: number;
  runStartId: number;
  firstUncommitted?: number;
  growing?: boolean;
  openId: number;
  startTime90k: number;
  endTime90k: number;
  videoSamples: number;
  sampleFileBytes: number;
  width: number;
  height: number;
  aspectWidth: number;
  aspectHeight: number;
  endReason?: string;
}

/**
 * Combines recordings, which are assumed to already be sorted in descending
 * chronological order.
 *
 * This is exported only for testing.
 */
export function combine(
  split90k: number | undefined,
  response: api.RecordingsResponse
): CombinedRecording[] {
  let out = [];
  let cur = null;

  for (const r of response.recordings) {
    const vse = response.videoSampleEntries[r.videoSampleEntryId];

    // Combine `r` into `cur` if `r` precedes `cur`, shouldn't be split, and
    // has similar resolution. It doesn't have to have exactly the same
    // video sample entry; minor changes to encoding can be seamlessly
    // combined into one `.mp4` file.
    if (
      cur !== null &&
      r.openId === cur.openId &&
      r.runStartId === cur.runStartId &&
      (r.endId ?? r.startId) + 1 === cur.startId &&
      cur.width === vse.width &&
      cur.height === vse.height &&
      cur.aspectWidth === vse.aspectWidth &&
      cur.aspectHeight === vse.aspectHeight &&
      (split90k === undefined || cur.endTime90k - r.startTime90k <= split90k)
    ) {
      cur.startId = r.startId;
      cur.firstUncommitted == r.firstUncommitted ?? cur.firstUncommitted;
      cur.startTime90k = r.startTime90k;
      cur.videoSamples += r.videoSamples;
      cur.sampleFileBytes += r.sampleFileBytes;
      continue;
    }

    // Otherwise, start a new `cur`, flushing any existing one.
    if (cur !== null) {
      out.push(cur);
    }
    cur = {
      startId: r.startId,
      endId: r.endId ?? r.startId,
      runStartId: r.runStartId,
      firstUncommitted: r.firstUncommitted,
      growing: r.growing,
      openId: r.openId,
      startTime90k: r.startTime90k,
      endTime90k: r.endTime90k,
      videoSamples: r.videoSamples,
      sampleFileBytes: r.sampleFileBytes,
      width: vse.width,
      height: vse.height,
      aspectWidth: vse.aspectWidth,
      aspectHeight: vse.aspectHeight,
      endReason: r.endReason,
    };
  }
  if (cur !== null) {
    out.push(cur);
  }
  return out;
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
  split90k?: number;
  response: { status: "skeleton" } | api.FetchResult<CombinedRecording[]>;
}

interface RowProps extends TableRowProps {
  start: React.ReactNode;
  end: React.ReactNode;
  endReason?: string;
  resolution: React.ReactNode;
  fps: React.ReactNode;
  storage: React.ReactNode;
  bitrate: React.ReactNode;
}

const Row = ({
  start,
  end,
  endReason,
  resolution,
  fps,
  storage,
  bitrate,
  ...rest
}: RowProps) => (
  <TableRow {...rest}>
    <TableCell align="right">{start}</TableCell>
    <TableCell align="right">
      {endReason !== undefined ? (
        <Tooltip title={endReason}>
          <Typography>{end}</Typography>
        </Tooltip>
      ) : (
        end
      )}
    </TableCell>
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
 * Creates a <tt>TableBody</tt> with a list of videos for a given
 * <tt>stream</tt> and <tt>range90k</tt>.
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
      clearTimeout(timerId);
      if (response.status === "success") {
        // Sort recordings in descending order.
        response.response.recordings.sort((a, b) => b.startId - a.startId);
        setState({
          range90k,
          split90k,
          response: {
            status: "success",
            response: combine(split90k, response.response),
          },
        });
      } else {
        setState({ range90k, split90k, response });
      }
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
    body = resp.map((r: CombinedRecording) => {
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
          onClick={() => setActiveRecording([stream, r])}
          start={formatTime(start)}
          end={formatTime(end)}
          endReason={r.endReason}
          resolution={`${r.width}x${r.height}`}
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
