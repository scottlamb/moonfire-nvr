// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import { createTheme, ThemeProvider } from "@mui/material/styles";
import { render } from "@testing-library/react";
import { SnackbarProvider } from "./snackbars";

export function renderWithCtx(
  children: React.ReactElement
): Pick<ReturnType<typeof render>, "rerender"> {
  function wrapped(children: React.ReactElement): React.ReactElement {
    return (
      <ThemeProvider theme={createTheme()}>
        <SnackbarProvider autoHideDuration={5000}>{children}</SnackbarProvider>
      </ThemeProvider>
    );
  }
  const { rerender } = render(wrapped(children));
  return {
    rerender: (children: React.ReactElement) => rerender(wrapped(children)),
  };
}
