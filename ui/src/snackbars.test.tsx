// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import { render, screen } from "@testing-library/react";
import { useEffect } from "react";
import { SnackbarProvider, useSnackbars } from "./snackbars";

// Mock out timers.
beforeEach(() => jest.useFakeTimers());
afterEach(() => {
  jest.runOnlyPendingTimers();
  jest.useRealTimers();
});

test("notifications that time out", async () => {
  function AddSnackbar() {
    const snackbars = useSnackbars();
    useEffect(() => {
      snackbars.enqueue({ message: "message A" });
      snackbars.enqueue({ message: "message B" });
    });
    return null;
  }

  render(
    <SnackbarProvider autoHideDuration={5000}>
      <AddSnackbar />
    </SnackbarProvider>
  );

  // message A should be present immediately.
  expect(screen.queryByText(/message A/)).toBeInTheDocument();
  expect(screen.queryByText(/message B/)).not.toBeInTheDocument();

  // ...then start to close...
  jest.advanceTimersByTime(5000);
  expect(screen.queryByText(/message A/)).toBeInTheDocument();
  expect(screen.queryByText(/message B/)).not.toBeInTheDocument();

  // ...then it should close and message B should open...
  jest.runOnlyPendingTimers();
  expect(screen.queryByText(/message A/)).not.toBeInTheDocument();
  expect(screen.queryByText(/message B/)).toBeInTheDocument();

  // ...then message B should start to close...
  jest.advanceTimersByTime(5000);
  expect(screen.queryByText(/message A/)).not.toBeInTheDocument();
  expect(screen.queryByText(/message B/)).toBeInTheDocument();

  // ...then message B should fully close.
  jest.runOnlyPendingTimers();
  expect(screen.queryByText(/message A/)).not.toBeInTheDocument();
  expect(screen.queryByText(/message B/)).not.toBeInTheDocument();
});

// TODO: test dismiss.
// TODO: test that context never changes.
// TODO: test drop-on-enqueue.
// TODO: test drop-after-enqueue, with manual and automatic keys.
