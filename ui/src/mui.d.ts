// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

export {};

declare module "@mui/material/styles" {
  interface PaletteOptions {
    header: string;
    headerContrastText: string;
  }
  interface Palette {
    header: string;
    headerContrastText: string;
  }
}
