// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import Dialog from "@mui/material/Dialog";
import DialogActions from "@mui/material/DialogActions";
import DialogTitle from "@mui/material/DialogTitle";
import FormHelperText from "@mui/material/FormHelperText";
import TextField from "@mui/material/TextField";
import Button from "@mui/material/Button";
import React, { useEffect } from "react";
import * as api from "./api";
import { useSnackbars } from "./snackbars";
import Box from "@mui/material/Box";
import DialogContent from "@mui/material/DialogContent";
import InputAdornment from "@mui/material/InputAdornment";
import Typography from "@mui/material/Typography";
import AccountCircle from "@mui/icons-material/AccountCircle";
import Lock from "@mui/icons-material/Lock";

interface Props {
  open: boolean;
  onSuccess: () => void;
  handleClose: () => void;
}

/**
 * Dialog for logging in.
 *
 * This is similar to <a
 * href="https://github.com/mui-org/material-ui/tree/master/docs/src/pages/getting-started/templates/sign-in">the
 * material-ui sign-in template</a>. On 401 error, it displays an error near
 * the submit button; on other errors, it uses a (transient) snackbar.
 *
 * This doesn't quite follow Chromium's <a
 * href="https://www.chromium.org/developers/design-documents/create-amazing-password-forms">creating</a>
 * amazing password forms</a> recommendations: it doesn't prompt a navigation
 * event on success. It's simpler to not mess with the history, and the current
 * method appears to work with Chrome 88's built-in password manager. To be
 * revisited if this causes problems.
 *
 * {@param open} should be true only when not logged in.
 * {@param onSuccess} called when the user is successfully logged in and the
 * cookie is set. The caller will have to do a new top-level API request to
 * retrieve the CSRF token, as well as other data that wasn't available before
 * logging in.
 * {@param handleClose} called when a close was requested (by pressing escape
 * or clicking outside the dialog). If the top-level API request fails when
 * not logged in (the server is running without
 * <tt>--allow-unauthenticated-permissions</tt>), the caller may ignore this.
 */
const Login = ({ open, onSuccess, handleClose }: Props) => {
  const snackbars = useSnackbars();

  // This is a simple uncontrolled form; use refs.
  const usernameRef = React.useRef<HTMLInputElement>(null);
  const passwordRef = React.useRef<HTMLInputElement>(null);

  const [error, setError] = React.useState<string | null>(null);
  const [loading, setLoading] = React.useState<api.LoginRequest | null>(null);

  useEffect(() => {
    if (loading === null) {
      return;
    }
    const abort = new AbortController();
    const send = async (signal: AbortSignal) => {
      const response = await api.login(loading, { signal });
      switch (response.status) {
        case "aborted":
          break;
        case "error":
          if (response.httpStatus === 401) {
            setError(response.message);
          } else {
            snackbars.enqueue({
              message: response.message,
              key: "login-error",
            });
          }
          setLoading(null);
          break;
        case "success":
          setLoading(null);
          onSuccess();
      }
    };
    send(abort.signal);
    return () => {
      abort.abort();
    };
  }, [loading, onSuccess, snackbars]);

  const onSubmit = async (e: React.FormEvent<HTMLFormElement>) => {
    e.preventDefault();

    // Suppress duplicate login attempts when latency is high.
    if (loading !== null) {
      return;
    }
    setLoading({
      username: usernameRef.current!.value,
      password: passwordRef.current!.value,
    });
  };

  return (
    <Dialog
      onClose={handleClose}
      aria-labelledby="login-title"
      open={open}
      maxWidth="sm"
      fullWidth={true}
    >
      <DialogTitle id="login-title">
        Welcome back!
        <Typography variant="body2">Please login to Moonfire NVR.</Typography>
      </DialogTitle>
      <form onSubmit={onSubmit}>
        <DialogContent>
          <Box sx={{ display: "flex", flexDirection: "column", gap: 2 }}>
            <TextField
              id="username"
              label="Username"
              variant="outlined"
              required
              autoComplete="username"
              fullWidth
              error={error != null}
              inputRef={usernameRef}
              InputProps={{
                startAdornment: (
                  <InputAdornment position="start">
                    <AccountCircle />
                  </InputAdornment>
                ),
              }}
            />
            <TextField
              id="password"
              label="Password"
              variant="outlined"
              type="password"
              required
              autoComplete="current-password"
              fullWidth
              error={error != null}
              inputRef={passwordRef}
              InputProps={{
                startAdornment: (
                  <InputAdornment position="start">
                    <Lock />
                  </InputAdornment>
                ),
              }}
            />

            {/* reserve space for an error; show when there's something to see */}
            <FormHelperText>{error == null ? " " : error}</FormHelperText>
          </Box>
        </DialogContent>
        <DialogActions>
          <Button
            type="submit"
            variant="contained"
            color="secondary"
            loading={loading !== null}
          >
            Log in
          </Button>
        </DialogActions>
      </form>
    </Dialog>
  );
};

export default Login;
