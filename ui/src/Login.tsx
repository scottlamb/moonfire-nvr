// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import Avatar from "@mui/material/Avatar";
import Dialog from "@mui/material/Dialog";
import DialogActions from "@mui/material/DialogActions";
import DialogTitle from "@mui/material/DialogTitle";
import FormControl from "@mui/material/FormControl";
import FormHelperText from "@mui/material/FormHelperText";
import { useTheme } from "@mui/material/styles";
import TextField from "@mui/material/TextField";
import LockOutlinedIcon from "@mui/icons-material/LockOutlined";
import LoadingButton from "@mui/lab/LoadingButton";
import React, { useEffect } from "react";
import * as api from "./api";
import { useSnackbars } from "./snackbars";

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
  const theme = useTheme();
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
    let abort = new AbortController();
    const send = async (signal: AbortSignal) => {
      let response = await api.login(loading, { signal });
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
        <Avatar sx={{ backgroundColor: theme.palette.secondary.main }}>
          <LockOutlinedIcon />
        </Avatar>
        Log in
      </DialogTitle>

      <form onSubmit={onSubmit}>
        <FormControl error={error != null} fullWidth>
          <TextField
            id="username"
            label="Username"
            variant="filled"
            required
            autoComplete="username"
            fullWidth
            inputRef={usernameRef}
          />
          <TextField
            id="password"
            label="Password"
            variant="filled"
            type="password"
            required
            autoComplete="current-password"
            fullWidth
            inputRef={passwordRef}
          />

          {/* reserve space for an error; show when there's something to see */}
          <FormHelperText>{error == null ? " " : error}</FormHelperText>

          <DialogActions>
            <LoadingButton
              type="submit"
              variant="contained"
              color="secondary"
              loading={loading !== null}
            >
              Log in
            </LoadingButton>
          </DialogActions>
        </FormControl>
      </form>
    </Dialog>
  );
};

export default Login;
