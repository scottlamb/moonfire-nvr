// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2023 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import LoadingButton from "@mui/lab/LoadingButton";
import Button from "@mui/material/Button";
import Dialog from "@mui/material/Dialog";
import DialogActions from "@mui/material/DialogActions";
import DialogContent from "@mui/material/DialogContent";
import DialogTitle from "@mui/material/DialogTitle";
import TextField from "@mui/material/TextField";
import Radio from "@mui/material/Radio";
import RadioGroup from "@mui/material/RadioGroup";
import * as api from "../api";
import FormControlLabel from "@mui/material/FormControlLabel";
import Stack from "@mui/material/Stack";
import FormLabel from "@mui/material/FormLabel";
import Box from "@mui/material/Box";
import Checkbox from "@mui/material/Checkbox";
import FormGroup from "@mui/material/FormGroup";
import { useEffect, useState } from "react";
import Tooltip from "@mui/material/Tooltip";
import HelpOutline from "@mui/icons-material/HelpOutline";
import { useSnackbars } from "../snackbars";

interface Props {
  // UserWithId (for edit), null (for add), undefined (for closed).
  prior: api.UserWithId | null;
  csrf?: string;
  onClose: () => void;
  refetch: () => void;
}

type PasswordAction = "leave" | "clear" | "set";

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
  labelId: string;
  label: React.ReactNode;
  children: React.ReactNode;
}
const MyGroup = ({ label, labelId, children }: MyGroupProps) => (
  <Box sx={{ mt: 4, mb: 4 }}>
    <FormLabel id={labelId}>{label}</FormLabel>
    {children}
  </Box>
);

const PermissionsCheckboxes = (props: {
  permissions: api.Permissions;
  setPermissions: React.Dispatch<React.SetStateAction<api.Permissions>>;
}) => {
  const checkboxes = PERMISSION_CHECKBOXES.map((def) => (
    <FormControlLabel
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
      control={
        <Checkbox
          checked={props.permissions[def.propName]}
          onChange={(e) => {
            props.setPermissions((p) => ({
              ...p,
              [def.propName]: e.target.checked,
            }));
          }}
        />
      }
    />
  ));
  return <>{checkboxes}</>;
};

export default function AddEditDialog({
  prior,
  csrf,
  onClose,
  refetch,
}: Props): JSX.Element {
  const hasPassword =
    prior !== undefined && prior !== null && prior.user.password !== null;
  const [username, setUsername] = useState(prior?.user.username ?? "");
  const [passwordAction, setPasswordAction] = useState<PasswordAction>("leave");
  const [password, setPassword] = useState("");
  const [permissions, setPermissions] = useState(prior?.user.permissions ?? {});
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
      console.log(resp);
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
  return (
    <Dialog open={true} maxWidth="md" fullWidth>
      <DialogTitle>
        {prior === null ? "Add user" : `Edit user ${prior.user.username}`}
      </DialogTitle>
      <form
        onSubmit={() =>
          setReq({
            username: username,
            password:
              passwordAction === "leave"
                ? undefined
                : passwordAction === "set"
                ? password
                : null,
            permissions: permissions,
          })
        }
      >
        <DialogContent>
          <TextField
            id="id"
            label="id"
            variant="filled"
            disabled
            fullWidth
            value={prior?.id ?? "(new)"}
            InputLabelProps={{ shrink: true }}
          />
          <TextField
            id="username"
            label="Username"
            variant="filled"
            value={username}
            onChange={(e) => setUsername(e.target.value)}
            required
            fullWidth
          />
          <MyGroup labelId="password-label" label="Password">
            <RadioGroup
              aria-labelledby="password-label"
              value={passwordAction}
              onChange={(e) => {
                setPasswordAction(e.target.value as PasswordAction);
              }}
            >
              {hasPassword && (
                <>
                  <FormControlLabel
                    value="leave"
                    control={<Radio />}
                    label="Leave set"
                  />
                  <FormControlLabel
                    value="clear"
                    control={<Radio />}
                    label="Clear"
                  />
                </>
              )}
              {!hasPassword && (
                <FormControlLabel
                  value="leave"
                  control={<Radio />}
                  label="Leave unset"
                />
              )}
              <Stack direction="row">
                <FormControlLabel
                  value="set"
                  control={<Radio />}
                  label="Set to"
                />
                <TextField
                  id="set-password"
                  label="New password"
                  type="password"
                  autoComplete="new-password"
                  variant="filled"
                  fullWidth
                  value={password}
                  // TODO: it'd be nice to allow clicking even when disabled,
                  // set the password action to "set", and give it focus.
                  // I tried briefly and couldn't make it work quite right.
                  disabled={passwordAction !== "set"}
                  onChange={(e) => setPassword(e.target.value)}
                />
              </Stack>
            </RadioGroup>
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
              <PermissionsCheckboxes
                permissions={permissions}
                setPermissions={setPermissions}
              />
            </FormGroup>
          </MyGroup>
        </DialogContent>
        <DialogActions>
          <Button onClick={onClose}>Cancel</Button>
          <LoadingButton
            loading={false}
            color="secondary"
            variant="contained"
            type="submit"
          >
            {prior === null ? "Add" : "Edit"}
          </LoadingButton>
        </DialogActions>
      </form>
    </Dialog>
  );
}
