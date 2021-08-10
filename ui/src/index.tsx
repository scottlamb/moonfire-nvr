// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import CssBaseline from "@material-ui/core/CssBaseline";
import { ThemeProvider, createTheme } from "@material-ui/core/styles";
import StyledEngineProvider from "@material-ui/core/StyledEngineProvider";
import LocalizationProvider from "@material-ui/lab/LocalizationProvider";
import "@fontsource/roboto";
import React from "react";
import ReactDOM from "react-dom";
import App from "./App";
import ErrorBoundary from "./ErrorBoundary";
import { SnackbarProvider } from "./snackbars";
import AdapterDateFns from "@material-ui/lab/AdapterDateFns";
import "./index.css";

const theme = createTheme({
  palette: {
    primary: {
      main: "#000000",
    },
    secondary: {
      main: "#e65100",
    },
  },
});

ReactDOM.render(
  <React.StrictMode>
    <StyledEngineProvider injectFirst>
      <CssBaseline />
      <ThemeProvider theme={theme}>
        <ErrorBoundary>
          <LocalizationProvider dateAdapter={AdapterDateFns}>
            <SnackbarProvider autoHideDuration={5000}>
              <App />
            </SnackbarProvider>
          </LocalizationProvider>
        </ErrorBoundary>
      </ThemeProvider>
    </StyledEngineProvider>
  </React.StrictMode>,
  document.getElementById("root")
);
