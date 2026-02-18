// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import { ThemeProvider, createTheme } from "@mui/material/styles";
import { LocalizationProvider } from "@mui/x-date-pickers/LocalizationProvider";
import "@fontsource/roboto";
import React from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import ErrorBoundary from "./ErrorBoundary";
import { SnackbarProvider } from "./snackbars";
import { AdapterDateFns } from "@mui/x-date-pickers/AdapterDateFns";
import "./index.css";
import { HashRouter } from "react-router";
import CssBaseline from "@mui/material/CssBaseline";
import { grey } from "@mui/material/colors";

const theme = createTheme({
  cssVariables: {
    colorSchemeSelector: "data",
  },
  palette: {
    contrastThreshold: 4.5,
    header: "var(--mui-palette-primary-main)",
    headerContrastText: "var(--mui-palette-primary-contrastText)",
  },
  colorSchemes: {
    dark: {
      palette: {
        contrastThreshold: 4.5,
        primary: {
          main: grey[200],
        },
        header: grey[800],
        headerContrastText: "#ffffff",
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
    <ThemeProvider theme={theme}>
      <CssBaseline />
      <ErrorBoundary>
        <LocalizationProvider dateAdapter={AdapterDateFns}>
          <SnackbarProvider autoHideDuration={5000}>
            <HashRouter>
              <App />
            </HashRouter>
          </SnackbarProvider>
        </LocalizationProvider>
      </ErrorBoundary>
    </ThemeProvider>
  </React.StrictMode>,
);
