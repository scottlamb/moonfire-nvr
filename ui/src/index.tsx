// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import CssBaseline from "@mui/material/CssBaseline";
import {
  Experimental_CssVarsProvider,
  experimental_extendTheme,
} from "@mui/material/styles";
import StyledEngineProvider from "@mui/material/StyledEngineProvider";
import { LocalizationProvider } from "@mui/x-date-pickers/LocalizationProvider";
import "@fontsource/roboto";
import React from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import ErrorBoundary from "./ErrorBoundary";
import { SnackbarProvider } from "./snackbars";
import { AdapterDateFns } from "@mui/x-date-pickers/AdapterDateFns";
import "./index.css";
import { HashRouter } from "react-router-dom";
import ThemeMode from "./components/ThemeMode";

const themeExtended = experimental_extendTheme({
  colorSchemes: {
    dark: {
      palette: {
        primary: {
          main: "#000000",
        },
        secondary: {
          main: "#e65100",
        },
      },
    },
  },
});
const container = document.getElementById("root");
const root = createRoot(container!);
root.render(
  <React.StrictMode>
    <StyledEngineProvider injectFirst>
      {/* <ThemeProvider theme={theme}> */}
      <Experimental_CssVarsProvider defaultMode="system" theme={themeExtended}>
        <CssBaseline />
        <ThemeMode>
          <ErrorBoundary>
            <LocalizationProvider dateAdapter={AdapterDateFns}>
              <SnackbarProvider autoHideDuration={5000}>
                <HashRouter>
                  <App />
                </HashRouter>
              </SnackbarProvider>
            </LocalizationProvider>
          </ErrorBoundary>
        </ThemeMode>
      </Experimental_CssVarsProvider>
      {/* </ThemeProvider> */}
    </StyledEngineProvider>
  </React.StrictMode>
);
