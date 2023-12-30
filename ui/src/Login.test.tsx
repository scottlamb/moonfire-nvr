// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import {
  screen,
  waitFor,
  waitForElementToBeRemoved,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { delay, http, HttpResponse } from "msw";
import { setupServer } from "msw/node";
import Login from "./Login";
import { renderWithCtx } from "./testutil";
import {
  beforeAll,
  afterEach,
  afterAll,
  test,
  vi,
  expect,
  beforeEach,
} from "vitest";

// Set up a fake API backend.
const server = setupServer(
  http.post<any, Record<string, string>>("/api/login", async ({ request }) => {
    const body = await request.json();
    const { username, password } = body!;
    console.log(
      "/api/login post username=" + username + " password=" + password
    );
    if (username === "slamb" && password === "hunter2") {
      return new HttpResponse(null, { status: 204 });
    } else if (username === "delay") {
      await delay("infinite");
      return new HttpResponse(null);
    } else if (username === "server-error") {
      return HttpResponse.text("server error", { status: 503 });
    } else if (username === "network-error") {
      return HttpResponse.error();
    } else {
      return HttpResponse.text("bad credentials", { status: 401 });
    }
  })
);
beforeAll(() => server.listen({ onUnhandledRequest: "error" }));
beforeEach(() => {
  // Using fake timers allows tests to jump forward to when a snackbar goes away, without incurring
  // extra real delay. msw only appears to work when `shouldAdvanceTime` is set though.
  vi.useFakeTimers({
    shouldAdvanceTime: true,
  });
});
afterEach(() => {
  vi.runOnlyPendingTimers();
  vi.useRealTimers();
  server.resetHandlers();
});
afterAll(() => server.close());

test("success", async () => {
  const user = userEvent.setup();
  const handleClose = vi.fn().mockName("handleClose");
  const onSuccess = vi.fn().mockName("handleOpen");
  renderWithCtx(
    <Login open={true} onSuccess={onSuccess} handleClose={handleClose} />
  );
  await user.type(screen.getByLabelText(/Username/), "slamb");
  await user.type(screen.getByLabelText(/Password/), "hunter2{enter}");
  await waitFor(() => expect(onSuccess).toHaveBeenCalledTimes(1));
});

test("close while pending", async () => {
  const user = userEvent.setup();
  const handleClose = vi.fn().mockName("handleClose");
  const onSuccess = vi.fn().mockName("handleOpen");
  const { rerender } = renderWithCtx(
    <Login open={true} onSuccess={onSuccess} handleClose={handleClose} />
  );
  await user.type(screen.getByLabelText(/Username/), "delay");
  await user.type(screen.getByLabelText(/Password/), "hunter2{enter}");
  expect(screen.getByRole("button", { name: /Log in/ })).toBeInTheDocument();
  rerender(
    <Login open={false} onSuccess={onSuccess} handleClose={handleClose} />
  );
  await waitFor(() =>
    expect(
      screen.queryByRole("button", { name: /Log in/ })
    ).not.toBeInTheDocument()
  );
});

test("bad credentials", async () => {
  const user = userEvent.setup();
  const handleClose = vi.fn().mockName("handleClose");
  const onSuccess = vi.fn().mockName("handleOpen");
  renderWithCtx(
    <Login open={true} onSuccess={onSuccess} handleClose={handleClose} />
  );
  await user.type(screen.getByLabelText(/Username/), "slamb");
  await user.type(screen.getByLabelText(/Password/), "wrong{enter}");
  await screen.findByText(/bad credentials/);
  expect(onSuccess).toHaveBeenCalledTimes(0);
});

test("server error", async () => {
  const user = userEvent.setup();
  const handleClose = vi.fn().mockName("handleClose");
  const onSuccess = vi.fn().mockName("handleOpen");
  renderWithCtx(
    <Login open={true} onSuccess={onSuccess} handleClose={handleClose} />
  );
  await user.type(screen.getByLabelText(/Username/), "server-error");
  await user.type(screen.getByLabelText(/Password/), "asdf{enter}");
  await screen.findByText(/server error/);
  vi.runOnlyPendingTimers();
  await waitForElementToBeRemoved(() => screen.queryByText(/server error/));
  expect(onSuccess).toHaveBeenCalledTimes(0);
});

test("network error", async () => {
  const user = userEvent.setup();
  const handleClose = vi.fn().mockName("handleClose");
  const onSuccess = vi.fn().mockName("handleOpen");
  renderWithCtx(
    <Login open={true} onSuccess={onSuccess} handleClose={handleClose} />
  );
  await user.type(screen.getByLabelText(/Username/), "network-error");
  await user.type(screen.getByLabelText(/Password/), "asdf{enter}");

  // This is the text chosen by msw:
  // https://github.com/mswjs/interceptors/blob/122a6533ce57d551dc3b59b3bb43a39026989b70/src/interceptors/fetch/index.ts#L187
  await screen.findByText(/Failed to fetch/);
  expect(onSuccess).toHaveBeenCalledTimes(0);
});
