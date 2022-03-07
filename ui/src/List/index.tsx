// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import Box from "@mui/material/Box";
import Modal from "@mui/material/Modal";
import Paper from "@mui/material/Paper";
import { Theme } from "@mui/material/styles";
import { makeStyles } from "@mui/styles";
import Table from "@mui/material/Table";
import TableContainer from "@mui/material/TableContainer";
import utcToZonedTime from "date-fns-tz/utcToZonedTime";
import format from "date-fns/format";
import React, { useMemo, useReducer, useState } from "react";
import * as api from "../api";
import { Stream } from "../types";
import DisplaySelector, { DEFAULT_DURATION } from "./DisplaySelector";
import StreamMultiSelector from "./StreamMultiSelector";
import TimerangeSelector from "./TimerangeSelector";
import VideoList from "./VideoList";
import { useLayoutEffect } from "react";
import { fillAspect } from "../aspect";
import useResizeObserver from "@react-hook/resize-observer";
import { useSearchParams } from "react-router-dom";
import { FrameProps } from "../App";
import IconButton from "@mui/material/IconButton";
import FilterList from "@mui/icons-material/FilterList";

const useStyles = makeStyles((theme: Theme) => ({
  root: {
    display: "flex",
    flexWrap: "wrap",
    margin: theme.spacing(2),
  },
  selectors: {
    width: "max-content",
    "& .MuiCard-root": {
      marginRight: theme.spacing(2),
      marginBottom: theme.spacing(2),
    },
  },
  videoTable: {
    flexGrow: 1,
    width: "max-content",
    height: "max-content",
    "& .streamHeader": {
      background: theme.palette.primary.light,
      color: theme.palette.primary.contrastText,
    },
    "& .MuiTableBody-root:not(:last-child):after": {
      content: "''",
      display: "block",
      height: theme.spacing(2),
    },
    "& tbody .recording": {
      cursor: "pointer",
    },
    "& .opt": {
      [theme.breakpoints.down("lg")]: {
        display: "none",
      },
    },
  },

  // When there's a video modal up, make the content as large as possible
  // without distorting it. Center it in the screen and ensure that the video
  // element only takes up the space actually used by the content, so that
  // clicking outside it will dismiss the modal.
  videoModal: {
    display: "flex",
    alignItems: "center",
    justifyContent: "center",
    "& video": {
      objectFit: "fill",
    },
  },
}));

interface FullScreenVideoProps {
  src: string;
  aspect: [number, number];
}

/**
 * A video sized for the entire document window constrained to aspect ratio.
 * This is particularly helpful for Firefox (89), which doesn't honor the
 * pixel aspect ratio specified in .mp4 files. Thus we need to specify it
 * out-of-band.
 */
const FullScreenVideo = ({ src, aspect }: FullScreenVideoProps) => {
  const ref = React.useRef<HTMLVideoElement>(null);
  useLayoutEffect(() => {
    fillAspect(document.body.getBoundingClientRect(), ref, aspect);
  });
  useResizeObserver(document.body, (entry: ResizeObserverEntry) => {
    fillAspect(entry.contentRect, ref, aspect);
  });
  return <video ref={ref} controls preload="auto" autoPlay src={src} />;
};

interface Props {
  timeZoneName: string;
  toplevel: api.ToplevelResponse;
  Frame: (props: FrameProps) => JSX.Element;
}

/// Parsed URL search parameters.
interface ParsedSearchParams {
  selectedStreamIds: Set<number>;
  split90k: number | undefined;
  trimStartAndEnd: boolean;
  timestampTrack: boolean;
}

/// <tt>ParsedSearchParams</tt> plus <tt>useState</tt>-like setters.
interface ParsedSearchParamsAndSetters extends ParsedSearchParams {
  setSelectedStreamIds: (selectedStreamIds: Set<number>) => void;
  setSplit90k: (split90k: number | undefined) => void;
  setTrimStartAndEnd: (trimStartAndEnd: boolean) => void;
  setTimestampTrack: (timestampTrack: boolean) => void;
}

const parseSearchParams = (raw: URLSearchParams): ParsedSearchParams => {
  let selectedStreamIds = new Set<number>();
  let split90k = DEFAULT_DURATION;
  let trimStartAndEnd = true;
  let timestampTrack = false;
  for (const [key, value] of raw) {
    switch (key) {
      case "s":
        selectedStreamIds.add(Number.parseInt(value, 10));
        break;
      case "split":
        split90k = value === "inf" ? undefined : Number.parseInt(value, 10);
        break;
      case "trim":
        trimStartAndEnd = value === "true";
        break;
      case "ts":
        timestampTrack = value === "true";
        break;
    }
  }
  return {
    selectedStreamIds,
    split90k,
    trimStartAndEnd,
    timestampTrack,
  };
};

const useParsedSearchParams = (): ParsedSearchParamsAndSetters => {
  const [search, setSearch] = useSearchParams();

  // This useMemo is necessary to avoid a re-rendering loop caused by each
  // call's selectedStreamIds set having different identity.
  const { selectedStreamIds, split90k, trimStartAndEnd, timestampTrack } =
    useMemo(() => parseSearchParams(search), [search]);

  const setSelectedStreamIds = (newSelectedStreamIds: Set<number>) => {
    // TODO: check if it's worth suppressing no-ops here.
    search.delete("s");
    for (const id of newSelectedStreamIds) {
      search.append("s", id.toString());
    }
    setSearch(search);
  };
  const setSplit90k = (newSplit90k: number | undefined) => {
    if (newSplit90k === split90k) {
      return;
    } else if (newSplit90k === DEFAULT_DURATION) {
      search.delete("split");
    } else if (newSplit90k === undefined) {
      search.set("split", "inf");
    } else {
      search.set("split", newSplit90k.toString());
    }
    setSearch(search);
  };
  const setTrimStartAndEnd = (newTrimStartAndEnd: boolean) => {
    if (newTrimStartAndEnd === trimStartAndEnd) {
      return;
    } else if (newTrimStartAndEnd === true) {
      search.delete("trim"); // default
    } else {
      search.set("trim", "false");
    }
    setSearch(search);
  };
  const setTimestampTrack = (newTimestampTrack: boolean) => {
    if (newTimestampTrack === timestampTrack) {
      return;
    } else if (newTimestampTrack === false) {
      search.delete("ts"); // default
    } else {
      search.set("ts", "true");
    }
    setSearch(search);
  };
  return {
    selectedStreamIds,
    setSelectedStreamIds,
    split90k,
    setSplit90k,
    trimStartAndEnd,
    setTrimStartAndEnd,
    timestampTrack,
    setTimestampTrack,
  };
};

const calcSelectedStreams = (
  toplevel: api.ToplevelResponse,
  ids: Set<number>
): Set<Stream> => {
  let streams = new Set<Stream>();
  for (const id of ids) {
    const s = toplevel.streams.get(id);
    if (s === undefined) {
      continue;
    }
    streams.add(s);
  }
  return streams;
};

const Main = ({ toplevel, timeZoneName, Frame }: Props) => {
  const classes = useStyles();

  const {
    selectedStreamIds,
    setSelectedStreamIds,
    split90k,
    setSplit90k,
    trimStartAndEnd,
    setTrimStartAndEnd,
    timestampTrack,
    setTimestampTrack,
  } = useParsedSearchParams();

  const [showSelectors, toggleShowSelectors] = useReducer(
    (m: boolean) => !m,
    true
  );

  // The time range to examine, or null if one hasn't yet been selected. This
  // is set by TimerangeSelector. As noted in TimerangeSelector's file-level
  // doc comment, its internal state changes don't always change range90k.
  // Other components operate on the end result to avoid unnecessary re-renders
  // and re-fetches.
  const [range90k, setRange90k] = useState<[number, number] | null>(null);

  // TimerangeSelector currently expects a Set<Stream>. Memoize one; otherwise
  // we'd get an infinite rerendering loop because the Set identity changes
  // each time.
  const selectedStreams = useMemo(
    () => calcSelectedStreams(toplevel, selectedStreamIds),
    [toplevel, selectedStreamIds]
  );

  const [activeRecording, setActiveRecording] = useState<
    [Stream, api.Recording, api.VideoSampleEntry] | null
  >(null);
  const formatTime = useMemo(() => {
    return (time90k: number) => {
      return format(
        utcToZonedTime(new Date(time90k / 90), timeZoneName),
        "d MMM yyyy HH:mm:ss"
      );
    };
  }, [timeZoneName]);

  let videoLists = [];
  for (const s of selectedStreams) {
    videoLists.push(
      <VideoList
        key={`${s.camera.uuid}-${s.streamType}`}
        stream={s}
        range90k={range90k}
        split90k={split90k}
        trimStartAndEnd={trimStartAndEnd}
        setActiveRecording={setActiveRecording}
        formatTime={formatTime}
      />
    );
  }
  const closeModal = (event: {}, reason: string) => {
    setActiveRecording(null);
  };
  const recordingsTable = (
    <TableContainer component={Paper} className={classes.videoTable}>
      <Table size="small">{videoLists}</Table>
    </TableContainer>
  );
  return (
    <Frame
      activityMenuPart={
        <IconButton
          aria-label="selectors"
          onClick={toggleShowSelectors}
          color="inherit"
          size="small"
        >
          <FilterList />
        </IconButton>
      }
    >
      <div className={classes.root}>
        <Box
          className={classes.selectors}
          sx={{ display: showSelectors ? "block" : "none" }}
        >
          <StreamMultiSelector
            toplevel={toplevel}
            selected={selectedStreamIds}
            setSelected={setSelectedStreamIds}
          />
          <TimerangeSelector
            selectedStreams={selectedStreams}
            setRange90k={setRange90k}
            timeZoneName={timeZoneName}
          />
          <DisplaySelector
            split90k={split90k}
            setSplit90k={setSplit90k}
            trimStartAndEnd={trimStartAndEnd}
            setTrimStartAndEnd={setTrimStartAndEnd}
            timestampTrack={timestampTrack}
            setTimestampTrack={setTimestampTrack}
          />
        </Box>
        {videoLists.length > 0 && recordingsTable}
        {activeRecording != null && (
          <Modal open onClose={closeModal} className={classes.videoModal}>
            <FullScreenVideo
              src={api.recordingUrl(
                activeRecording[0].camera.uuid,
                activeRecording[0].streamType,
                activeRecording[1],
                timestampTrack,
                trimStartAndEnd ? range90k! : undefined
              )}
              aspect={[
                activeRecording[2].aspectWidth,
                activeRecording[2].aspectHeight,
              ]}
            />
          </Modal>
        )}
      </div>
    </Frame>
  );
};

export default Main;
