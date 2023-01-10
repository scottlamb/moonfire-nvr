// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2023 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import { useFormContext } from "react-hook-form";
import {
  FormContainer,
  CheckboxElement,
  RadioButtonGroup,
  TextFieldElement,
} from "react-hook-form-mui";
import LoadingButton from "@mui/lab/LoadingButton";
import Button from "@mui/material/Button";
import Dialog from "@mui/material/Dialog";
import DialogActions from "@mui/material/DialogActions";
import DialogContent from "@mui/material/DialogContent";
import DialogTitle from "@mui/material/DialogTitle";
import TextField from "@mui/material/TextField";
import * as api from "../api";
import NewPassword from "../NewPassword";
import FormLabel from "@mui/material/FormLabel";
import Box from "@mui/material/Box";
import React, { useEffect, useState } from "react";
import Tooltip from "@mui/material/Tooltip";
import HelpOutline from "@mui/icons-material/HelpOutline";
import { useSnackbars } from "../snackbars";
import Collapse from "@mui/material/Collapse";
import FormGroup from "@mui/material/FormGroup";

interface Props {
  // UserWithId (for edit), null (for add), undefined (for closed).
  prior: api.UserWithId | null;
  csrf?: string;
  onClose: () => void;
  refetch: () => void;
}

interface PermissionCheckboxDefinition {
  propName: keyof api.Permissions;
  label: string;
  helpText?: string;
}

const PERMISSION_CHECKBOXES: PermissionCheckboxDefinition[] = [
  { propName: "adminUsers", label: "Administer users" },
  {
    propName: "readCameraConfigs",
    label: "Read camera configs",
    helpText:
      "Allow reading camera configs, including embedded credentials. Set for trusted users only.",
  },
  {
    propName: "updateSignals",
    label: "Update signals",
    helpText: "Allow updating 'signals' such as motion detection state.",
  },
  { propName: "viewVideo", label: "View video" },
];

// A group of form controls that's visually separated from the others.
interface MyGroupProps {
  labelId?: string;
  label?: React.ReactNode;
  children: React.ReactNode;
}
const MyGroup = ({ label, labelId, children }: MyGroupProps) => (
  <Box sx={{ mt: 4, mb: 4 }}>
    {label && <FormLabel id={labelId}>{label}</FormLabel>}
    {children}
  </Box>
);

const PermissionsCheckboxes = () => {
  const checkboxes = PERMISSION_CHECKBOXES.map((def) => (
    <CheckboxElement
      name={"permissions." + def.propName}
      key={"permissions." + def.propName}
      label={
        <>
          {def.label}
          {def.helpText && (
            <Tooltip title={def.helpText}>
              <HelpOutline />
            </Tooltip>
          )}
        </>
      }
    />
  ));
  return <>{checkboxes}</>;
};

interface FormData {
  username: string;
  passwordAction: "leave" | "clear" | "set";
  newPassword?: string;
  permissions: api.Permissions;
}

const MaybeNewPassword = () => {
  const { watch } = useFormContext<FormData>();
  const shown = watch("passwordAction") === "set";

  // It'd be nice to focus on the newPassword input when shown,
  // but react-hook-form-mui's <PasswordElement> uses inputRef for its own
  // purpose rather than plumbing through one we specify here, so I don't
  // see an easy way to do it without patching/bypassing that library.
  return (
    <Collapse in={shown}>
      <NewPassword required={shown} />
    </Collapse>
  );
};

export default function AddEditDialog({
  prior,
  csrf,
  onClose,
  refetch,
}: Props): JSX.Element {
  const hasPassword =
    prior !== undefined && prior !== null && prior.user.password !== null;
  const passwordOpts = hasPassword
    ? [
        { id: "leave", label: "Leave set" },
        { id: "clear", label: "Clear" },
        { id: "set", label: "Set a new password" },
      ]
    : [
        { id: "leave", label: "Leave unset" },
        { id: "set", label: "Set a new password" },
      ];
  const [req, setReq] = useState<api.UserSubset | undefined>();
  const snackbars = useSnackbars();
  useEffect(() => {
    const abort = new AbortController();
    const send = async (user: api.UserSubset, signal: AbortSignal) => {
      const resp = prior
        ? await api.updateUser(
            prior.id,
            {
              csrf: csrf,
              update: user,
            },
            { signal }
          )
        : await api.postUser(
            {
              csrf: csrf,
              user: user,
            },
            { signal }
          );
      setReq(undefined);
      switch (resp.status) {
        case "aborted":
          break;
        case "error":
          snackbars.enqueue({
            message: "Request failed: " + resp.message,
          });
          break;
        case "success":
          refetch();
          onClose();
          break;
      }
    };
    if (req !== undefined) {
      send(req, abort.signal);
    }
    return () => {
      abort.abort();
    };
  }, [prior, req, csrf, snackbars, onClose, refetch]);
  const onSuccess = (data: FormData) => {
    setReq({
      username: data.username,
      password: (() => {
        switch (data.passwordAction) {
          case "clear":
            return null;
          case "set":
            return data.newPassword;
          case "leave":
            return undefined;
        }
      })(),
      permissions: data.permissions,
    });
  };
  return (
    <Dialog open={true} maxWidth="md" fullWidth>
      <DialogTitle>
        {prior === null ? "Add user" : `Edit user ${prior.user.username}`}
      </DialogTitle>
      <FormContainer<FormData>
        defaultValues={{
          username: prior?.user.username,
          passwordAction: "leave",
          permissions: { ...prior?.user.permissions },
        }}
        onSuccess={onSuccess}
      >
        <DialogContent>
          <TextField
            name="id"
            label="id"
            variant="filled"
            disabled
            fullWidth
            value={prior?.id ?? "(new)"}
            InputLabelProps={{ shrink: true }}
            helperText=" "
          />
          <TextFieldElement
            name="username"
            label="Username"
            autoComplete="username"
            variant="filled"
            required
            fullWidth
            helperText=" "
          />
          <MyGroup>
            <RadioButtonGroup
              name="passwordAction"
              label="Password"
              options={passwordOpts}
              required
            />
            <MaybeNewPassword />
          </MyGroup>
          <MyGroup
            labelId="permissions-label"
            label={
              <>
                Permissions
                <Tooltip title="Permissions for new sessions created for this user. Currently changing a user's permissions does not affect existing sessions.">
                  <HelpOutline />
                </Tooltip>
              </>
            }
          >
            <FormGroup aria-labelledby="permissions-label">
              <PermissionsCheckboxes />
            </FormGroup>
          </MyGroup>
        </DialogContent>
        <DialogActions>
          <Button onClick={onClose}>Cancel</Button>
          <LoadingButton
            loading={req !== undefined}
            color="secondary"
            variant="contained"
            type="submit"
          >
            {prior === null ? "Add" : "Edit"}
          </LoadingButton>
        </DialogActions>
      </FormContainer>
    </Dialog>
  );
}
