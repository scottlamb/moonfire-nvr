// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2023 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import Button from "@mui/material/Button";
import Dialog from "@mui/material/Dialog";
import DialogActions from "@mui/material/DialogActions";
import DialogContent from "@mui/material/DialogContent";
import DialogTitle from "@mui/material/DialogTitle";
import { useEffect, useState } from "react";
import * as api from "../api";
import { useSnackbars } from "../snackbars";

interface Props {
  userToDelete?: api.UserWithId;
  csrf?: string;
  onClose: () => void;
  refetch: () => void;
}

export default function DeleteDialog({
  userToDelete,
  csrf,
  onClose,
  refetch,
}: Props): React.JSX.Element {
  const [req, setReq] = useState<undefined | number>();
  const snackbars = useSnackbars();
  useEffect(() => {
    const abort = new AbortController();
    const doFetch = async (id: number, signal: AbortSignal) => {
      const resp = await api.deleteUser(
        id,
        {
          csrf: csrf,
        },
        { signal },
      );
      setReq(undefined);
      switch (resp.status) {
        case "aborted":
          break;
        case "error":
          snackbars.enqueue({
            message: "Delete failed: " + resp.message,
          });
          break;
        case "success":
          refetch();
          onClose();
          break;
      }
    };
    if (req !== undefined) {
      doFetch(req, abort.signal);
    }
    return () => {
      abort.abort();
    };
  }, [req, csrf, snackbars, onClose, refetch]);
  return (
    <Dialog open={userToDelete !== undefined}>
      <DialogTitle>Delete user {userToDelete?.user.username}</DialogTitle>
      <DialogContent>
        This will permanently delete the given user and all associated sessions.
        There's no undo!
      </DialogContent>
      <DialogActions>
        <Button onClick={onClose} disabled={req !== undefined}>
          Cancel
        </Button>
        <Button
          loading={req !== undefined}
          onClick={() => setReq(userToDelete?.id)}
          color="secondary"
          variant="contained"
        >
          Delete
        </Button>
      </DialogActions>
    </Dialog>
  );
}
