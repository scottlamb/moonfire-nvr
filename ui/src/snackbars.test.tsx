// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import { act, render, screen, waitFor } from "@testing-library/react";
import { useEffect } from "react";
import { SnackbarProvider, useSnackbars } from "./snackbars";
import { beforeEach, afterEach, expect, test, vi } from "vitest";

// Mock out timers.
beforeEach(() => {
  vi.useFakeTimers();
});
afterEach(() => {
  vi.runOnlyPendingTimers();
  vi.useRealTimers();
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
  expect(screen.getByText(/message A/)).toBeInTheDocument();
  expect(screen.queryByText(/message B/)).not.toBeInTheDocument();

  // ...then start to close...
  act(() => vi.advanceTimersByTime(5000));
  expect(screen.getByText(/message A/)).toBeInTheDocument();
  expect(screen.queryByText(/message B/)).not.toBeInTheDocument();

  // ...then it should close and message B should open...
  act(() => vi.runOnlyPendingTimers());
  await waitFor(() =>
    expect(screen.queryByText(/message A/)).not.toBeInTheDocument()
  );
  expect(screen.getByText(/message B/)).toBeInTheDocument();

  // ...then message B should start to close...
  act(() => vi.advanceTimersByTime(5000));
  expect(screen.queryByText(/message A/)).not.toBeInTheDocument();
  expect(screen.getByText(/message B/)).toBeInTheDocument();

  // ...then message B should fully close.
  act(() => vi.runOnlyPendingTimers());
  expect(screen.queryByText(/message A/)).not.toBeInTheDocument();
  await waitFor(() =>
    expect(screen.queryByText(/message B/)).not.toBeInTheDocument()
  );
});

// TODO: test dismiss.
// TODO: test that context never changes.
// TODO: test drop-on-enqueue.
// TODO: test drop-after-enqueue, with manual and automatic keys.
