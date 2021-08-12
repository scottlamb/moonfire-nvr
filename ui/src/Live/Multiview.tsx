// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import Select, { SelectChangeEvent } from "@material-ui/core/Select";
import MenuItem from "@material-ui/core/MenuItem";
import React, { useReducer } from "react";
import { Camera } from "../types";
import { makeStyles } from "@material-ui/styles";
import { Theme } from "@material-ui/core/styles";

export interface Layout {
  className: string;
  cameras: number;
}

// These class names must match useStyles rules (below).
const LAYOUTS: Layout[] = [
  { className: "solo", cameras: 1 },
  { className: "main-plus-five", cameras: 6 },
  { className: "two-by-two", cameras: 4 },
  { className: "three-by-three", cameras: 9 },
];
const MAX_CAMERAS = 9;

const useStyles = makeStyles((theme: Theme) => ({
  root: {
    flex: "1 0 0",
    color: "white",
    marginTop: theme.spacing(2),
    overflow: "hidden",

    // TODO: this mid-level div can probably be removed.
    "& .mid": {
      width: "100%",
      height: "100%",
      position: "relative",
      display: "inline-block",
    },
  },
  inner: {
    // match parent's size without influencing it.
    position: "absolute",
    width: "100%",
    height: "100%",

    backgroundColor: "#000",
    overflow: "hidden",
    display: "grid",
    gridGap: "0px",

    // These class names must match LAYOUTS (above).
    "&.solo": {
      gridTemplateColumns: "100%",
      gridTemplateRows: "100%",
    },
    "&.two-by-two": {
      gridTemplateColumns: "repeat(2, calc(100% / 2))",
      gridTemplateRows: "repeat(2, calc(100% / 2))",
    },
    "&.main-plus-five, &.three-by-three": {
      gridTemplateColumns: "repeat(3, calc(100% / 3))",
      gridTemplateRows: "repeat(3, calc(100% / 3))",
    },
    "&.main-plus-five > div:nth-child(1)": {
      gridColumn: "span 2",
      gridRow: "span 2",
    },
  },
}));

export interface MultiviewProps {
  cameras: Camera[];
  layoutIndex: number;
  renderCamera: (camera: Camera | null, chooser: JSX.Element) => JSX.Element;
}

export interface MultiviewChooserProps {
  /// An index into <tt>LAYOUTS</tt>.
  layoutIndex: number;
  onChoice: (selectedIndex: number) => void;
}

/**
 * Chooses the layout for a Multiview.
 * Styled for placement in the app menu bar.
 */
export const MultiviewChooser = (props: MultiviewChooserProps) => {
  return (
    <Select
      id="layout"
      value={props.layoutIndex}
      onChange={(e) =>
        props.onChoice(
          typeof e.target.value === "string"
            ? parseInt(e.target.value)
            : e.target.value
        )
      }
      size="small"
      sx={{
        // Hacky attempt to style for the app menu.
        color: "inherit",
        "& svg": {
          color: "inherit",
        },
      }}
    >
      {LAYOUTS.map((e, i) => (
        <MenuItem key={e.className} value={i}>
          {e.className}
        </MenuItem>
      ))}
    </Select>
  );
};

/**
 * The cameras selected for the multiview.
 * This is always an array of length <tt>MAX_CAMERAS</tt>; only the first
 * LAYOUTS[layoutIndex].cameras are currently visible. There are no duplicates;
 * setting one element to a given camera unsets any others pointing to the same
 * camera.
 */
type SelectedCameras = Array<number | null>;

interface SelectOp {
  selectedIndex: number;
  cameraIndex: number | null;
}

function selectedReducer(old: SelectedCameras, op: SelectOp): SelectedCameras {
  let selected = [...old]; // shallow clone.
  if (op.cameraIndex !== null) {
    // de-dupe.
    for (let i = 0; i < selected.length; i++) {
      if (selected[i] === op.cameraIndex) {
        selected[i] = null;
      }
    }
  }
  selected[op.selectedIndex] = op.cameraIndex ?? null;
  return selected;
}

/**
 * Presents one or more camera views in one of several layouts.
 *
 * The parent should arrange for the multiview's outer div to be as large
 * as possible.
 */
const Multiview = (props: MultiviewProps) => {
  const [selected, updateSelected] = useReducer(
    selectedReducer,
    Array(MAX_CAMERAS).fill(null)
  );
  const outerRef = React.useRef<HTMLDivElement>(null);

  const classes = useStyles();
  const layout = LAYOUTS[props.layoutIndex];
  const monoviews = selected.slice(0, layout.cameras).map((e, i) => {
    // When a camera is selected, use the camera's index as the key.
    // This allows swapping cameras' positions without tearing down their
    // WebSocket connections and buffers.
    //
    // When no camera is selected, use the index within selected. (Actually,
    // its negation, to disambiguate between the two cases.)
    const key = e ?? -i;
    return (
      <Monoview
        key={key}
        cameras={props.cameras}
        cameraIndex={e}
        renderCamera={props.renderCamera}
        onSelect={(cameraIndex) =>
          updateSelected({ selectedIndex: i, cameraIndex })
        }
      />
    );
  });
  return (
    <div className={classes.root} ref={outerRef}>
      <div className="mid">
        <div className={`${classes.inner} ${layout.className}`}>
          {monoviews}
        </div>
      </div>
    </div>
  );
};

interface MonoviewProps {
  cameras: Camera[];
  cameraIndex: number | null;
  onSelect: (cameraIndex: number | null) => void;
  renderCamera: (camera: Camera | null, chooser: JSX.Element) => JSX.Element;
}

/** A single pane of a Multiview, including its camera chooser. */
const Monoview = (props: MonoviewProps) => {
  const handleChange = (event: SelectChangeEvent<number | null>) => {
    const {
      target: { value },
    } = event;
    props.onSelect(typeof value === "string" ? parseInt(value) : value);
  };
  const chooser = (
    <Select
      value={props.cameraIndex == null ? undefined : props.cameraIndex}
      onChange={handleChange}
      displayEmpty
      size="small"
      sx={{
        // Restyle to fit over the video (or black).
        backgroundColor: "rgba(255, 255, 255, 0.5)",
        "& svg": {
          color: "inherit",
        },
      }}
    >
      <MenuItem value={undefined}>(none)</MenuItem>
      {props.cameras.map((e, i) => (
        <MenuItem key={i} value={i}>
          {e.shortName}
        </MenuItem>
      ))}
    </Select>
  );
  return props.renderCamera(
    props.cameraIndex === null ? null : props.cameras[props.cameraIndex],
    chooser
  );
};

export default Multiview;
