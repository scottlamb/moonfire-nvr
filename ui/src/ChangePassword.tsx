// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2022 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import LoadingButton from "@mui/lab/LoadingButton";
import Dialog from "@mui/material/Dialog";
import DialogActions from "@mui/material/DialogActions";
import DialogContent from "@mui/material/DialogContent";
import DialogTitle from "@mui/material/DialogTitle";
import TextField from "@mui/material/TextField";
import React from "react";
import * as api from "./api";
import { useSnackbars } from "./snackbars";

interface Props {
  user: api.ToplevelUser;
  open: boolean;
  handleClose: () => void;
}

interface Request {
  userId: number;
  csrf: string;
  currentPassword: string;
  newPassword: string;
}

// Minimum password length, taken from [NIST
// guidelines](https://pages.nist.gov/800-63-3/sp800-63b.html), section 5.1.1.
// This is enforced on the frontend for now; a user who really wants to violate
// the rule can via API request.
const MIN_PASSWORD_LENGTH = 8;

/**
 * Dialog for changing password.
 *
 * There's probably a good set of best practices and even libraries for form
 * validation. I don't know them, but I played with a few similar forms, and
 * this code tries to behave similarly:
 *
 * - current password if the server has said the value is wrong and the form
 *   value hasn't changed.
 * - new password on blurring the field or submit attempt if it doesn't meet
 *   validation rules (as opposed to showing errors while you're typing),
 *   cleared as soon as validation succeeds.
 * - confirm password when new password changes away (unless confirm is empty),
 *   on blur, or on submit, cleared any time validation succeeds.
 *
 * The submit button is greyed on new/confirm password error. So it's initially
 * clickable (to give you the idea of what to do) but will complain more visibly
 * if you don't fill fields correctly first.
 */
const ChangePassword = ({ user, open, handleClose }: Props) => {
  const snackbars = useSnackbars();
  const [loading, setLoading] = React.useState<Request | null>(null);
  const [currentPassword, setCurrentPassword] = React.useState("");
  const [currentError, setCurrentError] = React.useState(false);
  const [newPassword, setNewPassword] = React.useState<string>("");
  const [newError, setNewError] = React.useState(false);
  const [confirmPassword, setConfirmPassword] = React.useState<string>("");
  const [confirmError, setConfirmError] = React.useState(false);
  React.useEffect(() => {
    if (loading === null) {
      return;
    }
    let abort = new AbortController();
    const send = async (signal: AbortSignal) => {
      let response = await api.updateUser(
        loading.userId,
        {
          csrf: loading.csrf,
          precondition: {
            password: loading.currentPassword,
          },
          update: {
            password: loading.newPassword,
          },
        },
        { signal }
      );
      switch (response.status) {
        case "aborted":
          break;
        case "error":
          if (response.httpStatus === 412) {
            if (currentPassword === loading.currentPassword) {
              setCurrentError(true);
            }
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
          snackbars.enqueue({
            message: "Password changed successfully",
            key: "password-changed",
          });
          handleClose();
      }
    };
    send(abort.signal);
    return () => {
      abort.abort();
    };
  }, [loading, handleClose, snackbars, currentPassword]);
  const onSubmit = (e: React.FormEvent<HTMLFormElement>) => {
    e.preventDefault();

    if (newPassword.length < MIN_PASSWORD_LENGTH) {
      setNewError(true);
      return;
    } else if (confirmPassword !== newPassword) {
      setConfirmError(true);
      return;
    }

    // Suppress concurrent attempts.
    if (loading !== null) {
      return;
    }
    setLoading({
      userId: user.id,
      csrf: user.session!.csrf,
      currentPassword: currentPassword,
      newPassword: newPassword,
    });
  };

  const onChangeNewPassword = (
    e: React.ChangeEvent<HTMLTextAreaElement | HTMLInputElement>
  ) => {
    setNewPassword(e.target.value);
    if (e.target.value.length >= MIN_PASSWORD_LENGTH) {
      setNewError(false);
    }
    if (e.target.value === confirmPassword) {
      setConfirmError(false);
    }
  };
  const onBlurNewPassword = () => {
    if (newPassword.length < MIN_PASSWORD_LENGTH) {
      setNewError(true);
    }
    if (newPassword !== confirmPassword && confirmPassword !== "") {
      setConfirmError(true);
    }
  };
  const onChangeConfirmPassword = (
    e: React.ChangeEvent<HTMLTextAreaElement | HTMLInputElement>
  ) => {
    setConfirmPassword(e.target.value);
    if (e.target.value === newPassword) {
      setConfirmError(false);
    }
  };
  const onBlurConfirmPassword = () => {
    if (confirmPassword !== newPassword) {
      setConfirmError(true);
    }
  };

  return (
    <Dialog
      onClose={handleClose}
      aria-labelledby="change-password-title"
      open={open}
      maxWidth="sm"
      fullWidth={true}
    >
      <DialogTitle id="change-password-title">
        Change password for {user.name}
      </DialogTitle>

      <form onSubmit={onSubmit}>
        <DialogContent>
          {/* The username is here in the hopes it will help password managers
           * find the correct entry. It's otherwise unused. */}
          <input
            name="username"
            type="hidden"
            value={user.name}
            autoComplete="username"
          />

          <TextField
            name="current-password"
            label="Current password"
            variant="filled"
            type="password"
            required
            autoComplete="current-password"
            fullWidth
            error={currentError}
            helperText={currentError ? "Current password is incorrect" : " "}
            value={currentPassword}
            onChange={(e) => {
              setCurrentError(false);
              setCurrentPassword(e.target.value);
            }}
          />
          <TextField
            name="new-password"
            label="New password"
            variant="filled"
            type="password"
            required
            autoComplete="new-password"
            value={newPassword}
            inputProps={{ minLength: MIN_PASSWORD_LENGTH }}
            error={newError}
            helperText={`Password must be at least ${MIN_PASSWORD_LENGTH} characters`}
            fullWidth
            onChange={onChangeNewPassword}
            onBlur={onBlurNewPassword}
          />
          <TextField
            name="confirm-new-password"
            label="Confirm new password"
            variant="filled"
            type="password"
            required
            autoComplete="new-password"
            value={confirmPassword}
            inputProps={{ minLength: MIN_PASSWORD_LENGTH }}
            fullWidth
            error={confirmError}
            helperText="Passwords must match."
            onChange={onChangeConfirmPassword}
            onBlur={onBlurConfirmPassword}
          />
        </DialogContent>

        <DialogActions>
          <LoadingButton
            type="submit"
            variant="contained"
            color="secondary"
            loading={loading !== null}
            disabled={newError || confirmError}
          >
            Change
          </LoadingButton>
        </DialogActions>
      </form>
    </Dialog>
  );
};

export default ChangePassword;
