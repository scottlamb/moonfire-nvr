// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import Card from "@material-ui/core/Card";
import Checkbox from "@material-ui/core/Checkbox";
import InputLabel from "@material-ui/core/InputLabel";
import FormControl from "@material-ui/core/FormControl";
import MenuItem from "@material-ui/core/MenuItem";
import Select from "@material-ui/core/Select";
import React from "react";
import { useTheme } from "@material-ui/core/styles";
import FormControlLabel from "@material-ui/core/FormControlLabel";

interface Props {
  split90k?: number;
  setSplit90k: (split90k?: number) => void;
  trimStartAndEnd: boolean;
  setTrimStartAndEnd: (trimStartAndEnd: boolean) => void;
  timestampTrack: boolean;
  setTimestampTrack: (timestampTrack: boolean) => void;
}

const DURATIONS: [string, number | undefined][] = [
  ["1 hour", 60 * 60 * 90000],
  ["4 hours", 4 * 60 * 60 * 90000],
  ["24 hours", 24 * 60 * 60 * 90000],
  ["infinite", undefined],
];

export const DEFAULT_DURATION = DURATIONS[0][1];

/**
 * Returns a card for setting options relating to how videos are displayed.
 */
const DisplaySelector = (props: Props) => {
  const theme = useTheme();
  return (
    <Card
      sx={{
        padding: theme.spacing(1),
        display: "flex",
        flexDirection: "column",
      }}
    >
      <FormControl fullWidth variant="outlined">
        <InputLabel id="split90k-label" shrink>
          Max video duration
        </InputLabel>
        <Select
          labelId="split90k-label"
          label="Max video duration"
          id="split90k"
          size="small"
          value={props.split90k}
          onChange={(e) =>
            props.setSplit90k(
              typeof e.target.value === "string"
                ? parseInt(e.target.value)
                : e.target.value
            )
          }
          displayEmpty
        >
          {DURATIONS.map(([l, d]) => (
            <MenuItem key={l} value={d}>
              {l}
            </MenuItem>
          ))}
        </Select>
      </FormControl>
      <FormControlLabel
        title="Trim each segment of video so that it is fully
    contained within the select time range. When this is not selected,
    all segments will overlap with the selected time range but may start
    and/or end outside it."
        control={
          <Checkbox
            checked={props.trimStartAndEnd}
            size="small"
            onChange={(event) => props.setTrimStartAndEnd(event.target.checked)}
            name="trim-start-and-end"
            color="secondary"
          />
        }
        label="Trim start and end"
      />
      <FormControlLabel
        title="Include a text track in each .mp4 with the
    timestamp at which the video was recorded."
        control={
          <Checkbox
            checked={props.timestampTrack}
            size="small"
            onChange={(event) => props.setTimestampTrack(event.target.checked)}
            name="timestamp-track"
            color="secondary"
          />
        }
        label="Timestamp track"
      />
    </Card>
  );
};

export default DisplaySelector;
