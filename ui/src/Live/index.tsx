// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import Container from "@mui/material/Container";
import ErrorIcon from "@mui/icons-material/Error";
import { Camera } from "../types";
import LiveCamera, { MediaSourceApi } from "./LiveCamera";
import Multiview, { MultiviewChooser } from "./Multiview";
import { FrameProps } from "../App";
import { useSearchParams } from "react-router-dom";
import { useEffect, useState } from "react";

export interface LiveProps {
  cameras: Camera[];
  Frame: (props: FrameProps) => JSX.Element;
}

const Live = ({ cameras, Frame }: LiveProps) => {
  const [searchParams, setSearchParams] = useSearchParams();

  const [multiviewLayoutIndex, setMultiviewLayoutIndex] = useState(
    Number.parseInt(
      searchParams.get("layout") ||
        localStorage.getItem("multiviewLayoutIndex") ||
        "0",
      10,
    ),
  );

  useEffect(() => {
    if (searchParams.has("layout"))
      localStorage.setItem(
        "multiviewLayoutIndex",
        searchParams.get("layout") || "0",
      );
  }, [searchParams]);

  const mediaSourceApi = MediaSourceApi;
  if (mediaSourceApi === undefined) {
    return (
      <Frame>
        <Container>
          <ErrorIcon
            sx={{
              float: "left",
              color: "secondary.main",
              marginRight: "1em",
            }}
          />
          Live view doesn't work yet on your browser. See{" "}
          <a href="https://github.com/scottlamb/moonfire-nvr/issues/121">
            #121
          </a>
          .
        </Container>
      </Frame>
    );
  }
  return (
    <Frame
      activityMenuPart={
        <MultiviewChooser
          layoutIndex={multiviewLayoutIndex}
          onChoice={(value) => {
            setMultiviewLayoutIndex(value);
            setSearchParams({ layout: value.toString() });
          }}
        />
      }
    >
      <Multiview
        layoutIndex={multiviewLayoutIndex}
        cameras={cameras}
        renderCamera={(camera: Camera | null, chooser: JSX.Element) => (
          <LiveCamera
            mediaSourceApi={mediaSourceApi}
            camera={camera}
            chooser={chooser}
          />
        )}
      />
    </Frame>
  );
};

export default Live;
