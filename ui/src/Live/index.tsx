// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import Container from "@material-ui/core/Container";
import ErrorIcon from "@material-ui/icons/Error";
import { Camera } from "../types";
import LiveCamera from "./LiveCamera";
import Multiview from "./Multiview";

export interface LiveProps {
  cameras: Camera[];
  layoutIndex: number;
}

const Live = ({ cameras, layoutIndex }: LiveProps) => {
  if ("MediaSource" in window === false) {
    return (
      <Container>
        <ErrorIcon
          sx={{
            float: "left",
            color: "secondary.main",
            marginRight: "1em",
          }}
        />
        Live view doesn't work yet on your browser. See{" "}
        <a href="https://github.com/scottlamb/moonfire-nvr/issues/121">#121</a>.
      </Container>
    );
  }
  return (
    <Multiview
      layoutIndex={layoutIndex}
      cameras={cameras}
      renderCamera={(camera: Camera | null, chooser: JSX.Element) => (
        <LiveCamera camera={camera} chooser={chooser} />
      )}
    />
  );
};

export { MultiviewChooser } from "./Multiview";
export default Live;
