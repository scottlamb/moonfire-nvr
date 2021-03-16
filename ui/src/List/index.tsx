// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import React, { useMemo, useState } from "react";
import { Camera, Stream } from "../types";
import * as api from "../api";
import VideoList from "./VideoList";
import { makeStyles, Theme } from "@material-ui/core/styles";
import Modal from "@material-ui/core/Modal";
import format from "date-fns/format";
import utcToZonedTime from "date-fns-tz/utcToZonedTime";
import Table from "@material-ui/core/Table";
import TableContainer from "@material-ui/core/TableContainer";
import Paper from "@material-ui/core/Paper";
import StreamMultiSelector from "./StreamMultiSelector";
import TimerangeSelector from "./TimerangeSelector";

const useStyles = makeStyles((theme: Theme) => ({
  root: {
    display: "flex",
    flexWrap: "wrap",
    margin: theme.spacing(2),
  },
  selectors: {
    marginRight: theme.spacing(2),
    marginBottom: theme.spacing(2),
    width: "max-content",
  },
  video: {
    objectFit: "contain",
    width: "100%",
    height: "100%",
    background: "#000",
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
}));

interface Props {
  timeZoneName: string;

  cameras: Camera[];
}

const Main = ({ cameras, timeZoneName }: Props) => {
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
        setActiveRecording={setActiveRecording}
        formatTime={formatTime}
      />
    );
  }
  const closeModal = (event: {}, reason: string) => {
    console.log("closeModal", reason);
    setActiveRecording(null);
  };
  const recordingsTable = (
    <TableContainer component={Paper} className={classes.videoTable}>
      <Table size="small">{videoLists}</Table>
    </TableContainer>
  );
  return (
    <div className={classes.root}>
      <div className={classes.selectors}>
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
      </div>
      {videoLists.length > 0 && recordingsTable}
      {activeRecording != null && (
        <Modal open onClose={closeModal}>
          <video
            controls
            preload="auto"
            autoPlay
            className={classes.video}
            src={api.recordingUrl(
              activeRecording[0].camera.uuid,
              activeRecording[0].streamType,
              activeRecording[1],
              range90k!
            )}
          />
        </Modal>
      )}
    </div>
  );
};

export default Main;
