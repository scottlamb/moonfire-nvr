// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import Box from "@mui/material/Box";
import Select, { SelectChangeEvent } from "@mui/material/Select";
import MenuItem from "@mui/material/MenuItem";
import React, { useCallback, useEffect, useReducer } from "react";
import { Camera } from "../types";
import { useSearchParams } from "react-router";
import IconButton from "@mui/material/IconButton";
import Tooltip from "@mui/material/Tooltip";
import Fullscreen from "@mui/icons-material/Fullscreen";

export interface Layout {
  className: string;
  cameras: number;
  name: string;
}

// These class names must match useStyles rules (below).
const LAYOUTS: Layout[] = [
  { className: "solo", cameras: 1, name: "1" },
  { className: "dual", cameras: 2, name: "2" },
  { className: "main-plus-five", cameras: 6, name: "Main + 5" },
  { className: "two-by-two", cameras: 4, name: "2x2" },
  { className: "three-by-three", cameras: 9, name: "3x3" },
];
const MAX_CAMERAS = 9;

export interface MultiviewProps {
  cameras: Camera[];
  layoutIndex: number;
  renderCamera: (
    camera: Camera | null,
    chooser: React.JSX.Element,
  ) => React.JSX.Element;
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
      onChange={(e) => {
        props.onChoice(
          typeof e.target.value === "string"
            ? parseInt(e.target.value)
            : e.target.value,
        );
      }}
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
          {e.name}
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
  const selected = [...old]; // shallow clone.
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
  const [searchParams, setSearchParams] = useSearchParams();

  const [selected, updateSelected] = useReducer(
    selectedReducer,
    searchParams.has("cams")
      ? JSON.parse(searchParams.get("cams") || "")
      : localStorage.getItem("camsSelected") !== null
        ? JSON.parse(localStorage.getItem("camsSelected") || "")
        : Array(MAX_CAMERAS).fill(null),
  );

  /**
   * Save previously selected cameras to local storage.
   */
  useEffect(() => {
    if (searchParams.has("cams"))
      localStorage.setItem("camsSelected", searchParams.get("cams") || "");
  }, [searchParams]);

  const outerRef = React.useRef<HTMLDivElement>(null);
  const layout = LAYOUTS[props.layoutIndex];

  /**
   * Toggle full screen.
   */
  const handleFullScreen = useCallback(() => {
    if (outerRef.current) {
      const elem = outerRef.current;
      if (document.fullscreenElement) {
        if (document.exitFullscreen) {
          document.exitFullscreen();
        }
      } else {
        if (elem.requestFullscreen) {
          elem.requestFullscreen();
        }
      }
    }
  }, [outerRef]);

  const monoviews = selected.slice(0, layout.cameras).map((e, i) => {
    // When a camera is selected, use the camera's index as the key.
    // This allows swapping cameras' positions without tearing down their
    // WebSocket connections and buffers.
    //
    // When no camera is selected, use the index within selected. (Actually,
    // -1 minus the index, to disambiguate between the two cases.)
    const key = e ?? -1 - i;

    return (
      <Monoview
        key={key}
        cameras={props.cameras}
        cameraIndex={e}
        renderCamera={props.renderCamera}
        onSelect={(cameraIndex) => {
          updateSelected({ selectedIndex: i, cameraIndex });
          searchParams.set(
            "cams",
            JSON.stringify(
              selectedReducer(selected, { selectedIndex: i, cameraIndex }),
            ),
          );
          setSearchParams(searchParams);
        }}
      />
    );
  });

  return (
    <Box
      ref={outerRef}
      sx={{
        flex: "1 0 0",
        color: "white",
        overflow: "hidden",

        // TODO: this mid-level div can probably be removed.
        "& > .mid": {
          width: "100%",
          height: "100%",
          position: "relative",
          display: "inline-block",
        },
      }}
    >
      <Tooltip title="Toggle full screen">
        <IconButton
          size="small"
          sx={{
            position: "fixed",
            background: "rgba(50,50,50,0.4) !important",
            transition: "0.2s",
            opacity: "0.4",
            bottom: 10,
            right: 10,
            zIndex: 9,
            color: "#fff",
            ":hover": {
              opacity: "1",
            },
          }}
          onClick={handleFullScreen}
        >
          <Fullscreen />
        </IconButton>
      </Tooltip>
      <div className="mid">
        <Box
          className={layout.className}
          sx={{
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
            "&.dual": {
              gridTemplateColumns: {
                xs: "100%",
                sm: "100%",
                md: "repeat(2, calc(100% / 2))",
              },
              gridTemplateRows: {
                xs: "50%",
                sm: "50%",
                md: "repeat(1, calc(100% / 1))",
              },
            },
            "&.two-by-two": {
              gridTemplateColumns: "repeat(2, calc(100% / 2))",
              gridTemplateRows: "repeat(2, calc(100% / 2))",
            },
            "&.main-plus-five, &.three-by-three": {
              gridTemplateColumns: "repeat(3, calc(100% / 3))",
              gridTemplateRows: "repeat(3, calc(100% / 3))",
            },
            "&.main-plus-five > div:nth-of-type(1)": {
              gridColumn: "span 2",
              gridRow: "span 2",
            },
          }}
        >
          {monoviews}
        </Box>
      </div>
    </Box>
  );
};

interface MonoviewProps {
  cameras: Camera[];
  cameraIndex: number | null;
  onSelect: (cameraIndex: number | null) => void;
  renderCamera: (
    camera: Camera | null,
    chooser: React.JSX.Element,
  ) => React.JSX.Element;
}

/** A single pane of a Multiview, including its camera chooser. */
const Monoview = (props: MonoviewProps) => {
  const handleChange = (event: SelectChangeEvent<string>) => {
    const {
      target: { value },
    } = event;

    props.onSelect(value === "null" ? null : parseInt(value));
  };

  const chooser = (
    <Select
      value={props.cameraIndex === null ? "null" : props.cameraIndex.toString()}
      onChange={handleChange}
      displayEmpty
      size="small"
      sx={{
        transform: "scale(0.8)",
        // Restyle to fit over the video (or black).
        backgroundColor: "rgba(50, 50, 50, 0.6)",
        boxShadow: "0 0 10px rgba(0, 0, 0, 0.4)",
        color: "#fff",
        "& svg": {
          color: "inherit",
        },
      }}
    >
      <MenuItem value="null">(none)</MenuItem>
      {props.cameras.map((e, i) => (
        <MenuItem key={i} value={i}>
          {e.shortName}
        </MenuItem>
      ))}
    </Select>
  );
  return props.renderCamera(
    props.cameraIndex === null ? null : props.cameras[props.cameraIndex],
    chooser,
  );
};

export default Multiview;
