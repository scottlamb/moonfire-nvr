// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2022 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import { useForm } from "react-hook-form";
import {
  FormContainer,
  PasswordElement,
  PasswordRepeatElement,
} from "react-hook-form-mui";
import Button from "@mui/material/Button";
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

interface FormData {
  currentPassword: string;
  newPassword: string;
}

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
  const formContext = useForm<FormData>();
  const setError = formContext.setError;
  const [loading, setLoading] = React.useState<Request | null>(null);
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
            setError("currentPassword", {
              message: "Incorrect password.",
            });
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
  }, [loading, handleClose, snackbars, setError]);
  const onSuccess = (data: FormData) => {
    // Suppress concurrent attempts.
    console.log("onSuccess", data);
    if (loading !== null) {
      return;
    }
    setLoading({
      userId: user.id,
      csrf: user.session!.csrf,
      currentPassword: data.currentPassword,
      newPassword: data.newPassword,
    });
  };

  return (
    <Dialog
      aria-labelledby="change-password-title"
      open={open}
      maxWidth="sm"
      fullWidth={true}
    >
      <DialogTitle id="change-password-title">Change password</DialogTitle>

      <FormContainer formContext={formContext} onSuccess={onSuccess}>
        <DialogContent>
          {/* The username is here in the hopes it will help password managers
           * find the correct entry. It's otherwise unused. */}
          <TextField
            name="username"
            label="Username"
            value={user.name}
            InputLabelProps={{ shrink: true }}
            disabled
            autoComplete="username"
            variant="filled"
            fullWidth
            helperText=" "
          />

          <PasswordElement
            name="currentPassword"
            label="Current password"
            variant="filled"
            type="password"
            required
            autoComplete="current-password"
            fullWidth
            helperText=" "
          />
          <PasswordElement
            name="newPassword"
            label="New password"
            variant="filled"
            required
            autoComplete="new-password"
            validation={{
              minLength: {
                value: MIN_PASSWORD_LENGTH,
                message: `Must have at least ${MIN_PASSWORD_LENGTH} characters`,
              },
            }}
            fullWidth
            helperText=" "
          />
          <PasswordRepeatElement
            name="confirmNewPassword"
            label="Confirm new password"
            variant="filled"
            type="password"
            passwordFieldName="newPassword"
            required
            autoComplete="new-password"
            fullWidth
            helperText=" "
          />
        </DialogContent>

        <DialogActions>
          <Button onClick={handleClose} disabled={loading !== null}>
            Cancel
          </Button>
          <LoadingButton
            type="submit"
            variant="contained"
            color="secondary"
            loading={loading !== null}
          >
            Change
          </LoadingButton>
        </DialogActions>
      </FormContainer>
    </Dialog>
  );
};

export default ChangePassword;
