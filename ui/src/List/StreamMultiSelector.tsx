// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import Card from "@mui/material/Card";
import { Camera, Stream, StreamType } from "../types";
import Checkbox from "@mui/material/Checkbox";
import { useTheme } from "@mui/material/styles";
import { makeStyles } from "@mui/styles";
import { ToplevelResponse } from "../api";

interface Props {
  toplevel: ToplevelResponse;
  selected: Set<Stream>;
  setSelected: (selected: Set<Stream>) => void;
}

const useStyles = makeStyles({
  streamSelectorTable: {
    fontSize: "0.9rem",
    "& td:first-child": {
      paddingRight: "3px",
    },
    "& td:not(:first-child)": {
      textAlign: "center",
    },
  },
  check: {
    padding: "3px",
  },
  "@media (pointer: fine)": {
    check: {
      padding: "0px",
    },
  },
});

/** Returns a table which allows selecting zero or more streams. */
const StreamMultiSelector = ({ toplevel, selected, setSelected }: Props) => {
  const theme = useTheme();
  const classes = useStyles();
  const setStream = (s: Stream, checked: boolean) => {
    const updated = new Set(selected);
    if (checked) {
      updated.add(s);
    } else {
      updated.delete(s);
    }
    setSelected(updated);
  };
  const toggleType = (st: StreamType) => {
    let updated = new Set(selected);
    let foundAny = false;
    for (const s of selected) {
      if (s.streamType === st) {
        updated.delete(s);
        foundAny = true;
      }
    }
    if (!foundAny) {
      for (const c of toplevel.cameras) {
        if (c.streams[st] !== undefined) {
          updated.add(c.streams[st as StreamType]!);
        }
      }
    }
    setSelected(updated);
  };
  const toggleCamera = (c: Camera) => {
    const updated = new Set(selected);
    let foundAny = false;
    for (const st in c.streams) {
      const s = c.streams[st as StreamType]!;
      if (selected.has(s)) {
        updated.delete(s);
        foundAny = true;
      }
    }
    if (!foundAny) {
      for (const st in c.streams) {
        updated.add(c.streams[st as StreamType]!);
      }
    }
    setSelected(updated);
  };

  const cameraRows = toplevel.cameras.map((c) => {
    function checkbox(st: StreamType) {
      const s = c.streams[st];
      if (s === undefined) {
        return (
          <Checkbox className={classes.check} color="secondary" disabled />
        );
      }
      return (
        <Checkbox
          className={classes.check}
          size="small"
          checked={selected.has(s)}
          color="secondary"
          onChange={(event) => setStream(s, event.target.checked)}
        />
      );
    }
    return (
      <tr key={c.uuid}>
        <td onClick={() => toggleCamera(c)}>{c.shortName}</td>
        <td>{checkbox("main")}</td>
        <td>{checkbox("sub")}</td>
      </tr>
    );
  });
  return (
    <Card
      sx={{
        padding: theme.spacing(1),
      }}
    >
      <table className={classes.streamSelectorTable}>
        <thead>
          <tr>
            <td />
            <td onClick={() => toggleType("main")}>main</td>
            <td onClick={() => toggleType("sub")}>sub</td>
          </tr>
        </thead>
        <tbody>{cameraRows}</tbody>
      </table>
    </Card>
  );
};

export default StreamMultiSelector;
