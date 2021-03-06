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
import TableCell from "@material-ui/core/TableCell";
import TableContainer from "@material-ui/core/TableContainer";
import TableHead from "@material-ui/core/TableHead";
import TableRow from "@material-ui/core/TableRow";
import Paper from "@material-ui/core/Paper";
import StreamMultiSelector from "./StreamMultiSelector";
import TimerangeSelector from "./TimerangeSelector";

const useStyles = makeStyles((theme: Theme) => ({
  root: {
    display: "flex",
    [theme.breakpoints.down("md")]: {
      flexDirection: "column",
    },
    margin: theme.spacing(2),
  },
  selectors: {
    [theme.breakpoints.up("md")]: {
      marginRight: theme.spacing(2),
    },
    [theme.breakpoints.down("md")]: {
      marginBottom: theme.spacing(2),
    },
  },
  video: {
    objectFit: "contain",
    width: "100%",
    height: "100%",
    background: "#000",
  },
  camera: {
    background: theme.palette.primary.light,
    color: theme.palette.primary.contrastText,
  },
  videoTable: {
    flexGrow: 1,
    "& .MuiTableBody-root:not(:last-child):after": {
      content: "''",
      display: "block",
      height: theme.spacing(2),
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
      <React.Fragment key={`${s.camera.uuid}-${s.streamType}`}>
        <TableHead>
          <TableRow>
            <TableCell colSpan={6} className={classes.camera}>
              {s.camera.shortName} {s.streamType}
            </TableCell>
          </TableRow>
          <TableRow>
            <TableCell>start</TableCell>
            <TableCell>end</TableCell>
            <TableCell>resolution</TableCell>
            <TableCell>fps</TableCell>
            <TableCell>storage</TableCell>
            <TableCell>bitrate</TableCell>
          </TableRow>
        </TableHead>
        <VideoList
          stream={s}
          range90k={range90k}
          setActiveRecording={setActiveRecording}
          formatTime={formatTime}
        />
      </React.Fragment>
    );
  }
  const closeModal = (event: {}, reason: string) => {
    console.log("closeModal", reason);
    setActiveRecording(null);
  };
  const recordingsTable = (
    <TableContainer component={Paper}>
      <Table size="small" className={classes.videoTable}>
        {videoLists}
      </Table>
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
              activeRecording[1]
            )}
          />
        </Modal>
      )}
    </div>
  );
};

export default Main;
