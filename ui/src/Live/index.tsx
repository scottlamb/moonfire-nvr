// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import { Camera } from "../types";
import LiveCamera from "./LiveCamera";
import Multiview from "./Multiview";

export interface LiveProps {
  cameras: Camera[];
  layoutIndex: number;
}

const Live = ({ cameras, layoutIndex }: LiveProps) => {
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
