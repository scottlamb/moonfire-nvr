// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2023 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import { PasswordElement } from "react-hook-form-mui";
import { useWatch } from "react-hook-form";

// Minimum password length, taken from [NIST
// guidelines](https://pages.nist.gov/800-63-3/sp800-63b.html), section 5.1.1.
// This is enforced on the frontend for now; a user who really wants to violate
// the rule can via API request.
const MIN_PASSWORD_LENGTH = 8;

/// Form elements for setting a new password, shared between the ChangePassword
/// dialog (for any user to change their own password) and AddEditDialog
/// (for admins to add/edit any user).
///
/// Does no validation if `!required`; AddEditDialog doesn't care about these
/// fields unless the password action is "set" (rather than "leave" or "clear").
export default function NewPassword(props: { required?: boolean }) {
  const required = props.required ?? true;
  const newPasswordValue = useWatch({ name: "newPassword" });
  return (
    <>
      <PasswordElement
        name="newPassword"
        label="New password"
        variant="filled"
        required={required}
        autoComplete="new-password"
        rules={{
          validate: (v: string) => {
            if (!required) {
              return true;
            } else if (v.length === 0) {
              return "New password is required.";
            } else if (v.length < MIN_PASSWORD_LENGTH) {
              return `Passwords must have at least ${MIN_PASSWORD_LENGTH} characters.`;
            } else {
              return true;
            }
          },
        }}
        fullWidth
        helperText=" "
      />
      <PasswordElement
        name="confirmNewPassword"
        label="Confirm new password"
        variant="filled"
        type="password"
        required={required}
        autoComplete="new-password"
        fullWidth
        helperText=" "
        rules={{
          validate: (v: string) => {
            if (!required) {
              return true;
            } else if (v.length === 0) {
              return "Must confirm new password.";
            } else if (v !== newPasswordValue) {
              return "Passwords must match.";
            } else {
              return true;
            }
          },
        }}
      />
    </>
  );
}
