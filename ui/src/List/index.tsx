// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import Box from "@material-ui/core/Box";
import Modal from "@material-ui/core/Modal";
import Paper from "@material-ui/core/Paper";
import { makeStyles, Theme } from "@material-ui/core/styles";
import Table from "@material-ui/core/Table";
import TableContainer from "@material-ui/core/TableContainer";
import utcToZonedTime from "date-fns-tz/utcToZonedTime";
import format from "date-fns/format";
import React, { useMemo, useState } from "react";
import * as api from "../api";
import { Camera, Stream } from "../types";
import DisplaySelector, { DEFAULT_DURATION } from "./DisplaySelector";
import StreamMultiSelector from "./StreamMultiSelector";
import TimerangeSelector from "./TimerangeSelector";
import VideoList from "./VideoList";

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
      objectFit: "contain",
      maxWidth: "100%",
      maxHeight: "100%",
    },
  },
}));

interface Props {
  timeZoneName: string;
  cameras: Camera[];
  showSelectors: boolean;
}

const Main = ({ cameras, timeZoneName, showSelectors }: Props) => {
  const classes = useStyles();

  /**
   * Selected streams to display and use for selecting time ranges.
   * This currently uses the <tt>Stream</tt> object, which means it will
   * not be stable across top-level API fetches. Maybe an id would be better.
   */
  const [selectedStreams, setSelectedStreams] = useState<Set<Stream>>(
    new Set()
  );

  /** Selected time range. */
  const [range90k, setRange90k] = useState<[number, number] | null>(null);

  const [split90k, setSplit90k] = useState(DEFAULT_DURATION);

  const [trimStartAndEnd, setTrimStartAndEnd] = useState(true);
  const [timestampTrack, setTimestampTrack] = useState(false);

  const [activeRecording, setActiveRecording] = useState<
    [Stream, api.Recording] | null
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
    <div className={classes.root}>
      <Box
        className={classes.selectors}
        sx={{ display: showSelectors ? "block" : "none" }}
      >
        <StreamMultiSelector
          cameras={cameras}
          selected={selectedStreams}
          setSelected={setSelectedStreams}
        />
        <TimerangeSelector
          selectedStreams={selectedStreams}
          range90k={range90k}
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
          <video
            controls
            preload="auto"
            autoPlay
            src={api.recordingUrl(
              activeRecording[0].camera.uuid,
              activeRecording[0].streamType,
              activeRecording[1],
              timestampTrack,
              trimStartAndEnd ? range90k! : undefined
            )}
          />
        </Modal>
      )}
    </div>
  );
};

export default Main;
