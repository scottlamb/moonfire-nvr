// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import Box from "@mui/material/Box";
import Card from "@mui/material/Card";
import { Camera, Stream, StreamType } from "../types";
import Checkbox from "@mui/material/Checkbox";
import { ToplevelResponse } from "../api";
import { CardContent } from "@mui/material";

interface Props {
  toplevel: ToplevelResponse;
  selected: Set<number>;
  setSelected: (selected: Set<number>) => void;
}

/** Returns a table which allows selecting zero or more streams. */
const StreamMultiSelector = ({ toplevel, selected, setSelected }: Props) => {
  const setStream = (s: Stream, checked: boolean) => {
    const updated = new Set(selected);
    if (checked) {
      updated.add(s.id);
    } else {
      updated.delete(s.id);
    }
    setSelected(updated);
  };
  const toggleType = (st: StreamType) => {
    let updated = new Set(selected);
    let foundAny = false;
    for (const sid of selected) {
      const s = toplevel.streams.get(sid);
      if (s === undefined) {
        continue;
      }
      if (s.streamType === st) {
        updated.delete(s.id);
        foundAny = true;
      }
    }
    if (!foundAny) {
      for (const c of toplevel.cameras) {
        if (c.streams[st] !== undefined) {
          updated.add(c.streams[st as StreamType]!.id);
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
      if (selected.has(s.id)) {
        updated.delete(s.id);
        foundAny = true;
      }
    }
    if (!foundAny) {
      for (const st in c.streams) {
        updated.add(c.streams[st as StreamType]!.id);
      }
    }
    setSelected(updated);
  };

  const cameraRows = toplevel.cameras.map((c) => {
    function checkbox(st: StreamType) {
      const s = c.streams[st];
      if (s === undefined) {
        return <Checkbox color="secondary" disabled />;
      }
      return (
        <Checkbox
          size="small"
          checked={selected.has(s.id)}
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
    >
      <CardContent>
      <Box
        component="table"
        sx={{
          fontSize: "0.9rem",
          "& td:first-of-type": {
            paddingRight: "3px",
          },
          "& td:not(:first-of-type)": {
            textAlign: "center",
          },
          "& .MuiCheckbox-root": {
            padding: "3px",
          },
          "@media (pointer: fine)": {
            "& .MuiCheckbox-root": {
              padding: "0px",
            },
          },
        }}
      >
        <thead>
          <tr>
            <td />
            <td onClick={() => toggleType("main")}>main</td>
            <td onClick={() => toggleType("sub")}>sub</td>
          </tr>
        </thead>
        <tbody>{cameraRows}</tbody>
        </Box>
      </CardContent>
    </Card>
  );
};

export default StreamMultiSelector;
