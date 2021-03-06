// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import CircularProgress from "@material-ui/core/CircularProgress";
import React from "react";
import * as api from "../api";
import { useSnackbars } from "../snackbars";
import { Stream } from "../types";
import TableBody from "@material-ui/core/TableBody";
import TableCell from "@material-ui/core/TableCell";
import TableRow from "@material-ui/core/TableRow";

interface Props {
  stream: Stream;
  range90k: [number, number] | null;
  setActiveRecording: (recording: [Stream, api.Recording] | null) => void;
  formatTime: (time90k: number) => string;
}

const frameRateFmt = new Intl.NumberFormat([], {
  maximumFractionDigits: 0,
});

const sizeFmt = new Intl.NumberFormat([], {
  maximumFractionDigits: 1,
});

/**
 * Creates a <tt>TableBody</tt> with a list of videos for a given
 * <tt>stream</tt> and <tt>range90k</tt>.
 *
 * The parent is responsible for creating the greater table.
 *
 * When one is clicked, calls <tt>setActiveRecording</tt>.
 */
const VideoList = ({
  stream,
  range90k,
  setActiveRecording,
  formatTime,
}: Props) => {
  const snackbars = useSnackbars();
  const [
    response,
    setResponse,
  ] = React.useState<api.FetchResult<api.RecordingsResponse> | null>(null);
  const [showLoading, setShowLoading] = React.useState(false);
  React.useEffect(() => {
    const abort = new AbortController();
    const doFetch = async (signal: AbortSignal, range90k: [number, number]) => {
      const req: api.RecordingsRequest = {
        cameraUuid: stream.camera.uuid,
        stream: stream.streamType,
        startTime90k: range90k[0],
        endTime90k: range90k[1],
      };
      setResponse(await api.recordings(req, { signal }));
    };
    if (range90k != null) {
      doFetch(abort.signal, range90k);
      const timeout = setTimeout(() => setShowLoading(true), 1000);
      return () => {
        abort.abort();
        clearTimeout(timeout);
      };
    }
  }, [range90k, snackbars, stream]);

  let body = null;
  if (response === null) {
    if (showLoading) {
      body = (
        <TableRow>
          <TableCell colSpan={6}>
            <CircularProgress />
          </TableCell>
        </TableRow>
      );
    }
  } else if (response.status === "error") {
    body = (
      <TableRow>
        <TableCell colSpan={6}>Error: {response.status}</TableCell>
      </TableRow>
    );
  } else if (response.status === "success") {
    const resp = response.response;
    body = resp.recordings.map((r: api.Recording) => {
      const vse = resp.videoSampleEntries[r.videoSampleEntryId];
      const durationSec = (r.endTime90k - r.startTime90k) / 90000;
      return (
        <TableRow
          key={r.startId}
          onClick={() => setActiveRecording([stream, r])}
        >
          <TableCell>{formatTime(r.startTime90k)}</TableCell>
          <TableCell>{formatTime(r.endTime90k)}</TableCell>
          <TableCell>
            {vse.width}x{vse.height}
          </TableCell>
          <TableCell>
            {frameRateFmt.format(r.videoSamples / durationSec)}
          </TableCell>
          <TableCell>
            {sizeFmt.format(r.sampleFileBytes / 1048576)} MiB
          </TableCell>
          <TableCell>
            {sizeFmt.format((r.sampleFileBytes / durationSec) * 0.000008)} Mbps
          </TableCell>
        </TableRow>
      );
    });
  }
  return <TableBody>{body}</TableBody>;
};

export default VideoList;
