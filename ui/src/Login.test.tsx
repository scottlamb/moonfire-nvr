// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { rest } from "msw";
import { setupServer } from "msw/node";
import Login from "./Login";
import { renderWithCtx } from "./testutil";

// Set up a fake API backend.
const server = setupServer(
  rest.post("/api/login", (req, res, ctx) => {
    const { username, password } = req.body! as Record<string, string>;
    console.log(
      "/api/login post username=" + username + " password=" + password
    );
    if (username === "slamb" && password === "hunter2") {
      return res(ctx.status(204));
    } else if (username === "delay") {
      return res(ctx.delay("infinite"));
    } else if (username === "server-error") {
      return res(ctx.status(503), ctx.text("server error"));
    } else if (username === "network-error") {
      return res.networkError("network error");
    } else {
      return res(ctx.status(401), ctx.text("bad credentials"));
    }
  })
);
beforeAll(() => server.listen({ onUnhandledRequest: "error" }));
afterEach(() => server.resetHandlers());
afterAll(() => server.close());

// TODO: fix this. It's meant to allow the snack bar timers to run quickly
// in tests, but fake timers seem to have problems:
//
// * react-scripts v4, @testing-library/react v11: worked
// * react-scripts v5, @testing-library/react v11:
//   saw "was not wrapped in act" warnings, test failed
//   (Seems like waitFor's internal advance calls aren't wrapped in act)
// * react-scripts v5, @testing-library/react v12:
//   msw requests never complete
//   https://github.com/mswjs/msw/issues/448#issuecomment-723438099 ?
//   https://github.com/facebook/jest/issues/11103 ?
//   https://github.com/facebook/jest/issues/13018 ?
//
// Argh!
// beforeEach(() => jest.useFakeTimers({
//   legacyFakeTimers: true,
// }));
// afterEach(() => {
//   act(() => {
//     jest.runOnlyPendingTimers();
//     jest.useRealTimers();
//   });
// });

test("success", async () => {
  const user = userEvent.setup();
  const handleClose = jest.fn().mockName("handleClose");
  const onSuccess = jest.fn().mockName("handleOpen");
  renderWithCtx(
    <Login open={true} onSuccess={onSuccess} handleClose={handleClose} />
  );
  await user.type(screen.getByLabelText(/Username/), "slamb");
  await user.type(screen.getByLabelText(/Password/), "hunter2{enter}");
  await waitFor(() => expect(onSuccess).toHaveBeenCalledTimes(1));
});

// TODO: fix and re-enable this test.
// Currently it makes "CI=true npm run test" hang.
// I think the problem is that npmjs doesn't really support aborting requests,
// so the delay("infinite") request just sticks around, even though the fetch
// has been aborted. Maybe https://github.com/mswjs/msw/pull/585 will fix it.
test.skip("close while pending", async () => {
  const handleClose = jest.fn().mockName("handleClose");
  const onSuccess = jest.fn().mockName("handleOpen");
  const { rerender } = renderWithCtx(
    <Login open={true} onSuccess={onSuccess} handleClose={handleClose} />
  );
  userEvent.type(screen.getByLabelText(/Username/), "delay");
  userEvent.type(screen.getByLabelText(/Password/), "hunter2{enter}");
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

// TODO: fix and re-enable this test.
// It depends on the timers; see TODO above.
test.skip("bad credentials", async () => {
  const handleClose = jest.fn().mockName("handleClose");
  const onSuccess = jest.fn().mockName("handleOpen");
  renderWithCtx(
    <Login open={true} onSuccess={onSuccess} handleClose={handleClose} />
  );
  await userEvent.type(screen.getByLabelText(/Username/), "slamb");
  await userEvent.type(screen.getByLabelText(/Password/), "wrong{enter}");
  await screen.findByText(/bad credentials/);
  expect(onSuccess).toHaveBeenCalledTimes(0);
});

// TODO: fix and re-enable this test.
// It depends on the timers; see TODO above.
test.skip("server error", async () => {
  const handleClose = jest.fn().mockName("handleClose");
  const onSuccess = jest.fn().mockName("handleOpen");
  renderWithCtx(
    <Login open={true} onSuccess={onSuccess} handleClose={handleClose} />
  );
  await userEvent.type(screen.getByLabelText(/Username/), "server-error");
  await userEvent.type(screen.getByLabelText(/Password/), "asdf{enter}");
  await screen.findByText(/server error/);
  await waitFor(() =>
    expect(screen.queryByText(/server error/)).not.toBeInTheDocument()
  );
  expect(onSuccess).toHaveBeenCalledTimes(0);
});

// TODO: fix and re-enable this test.
// It depends on the timers; see TODO above.
test.skip("network error", async () => {
  const handleClose = jest.fn().mockName("handleClose");
  const onSuccess = jest.fn().mockName("handleOpen");
  renderWithCtx(
    <Login open={true} onSuccess={onSuccess} handleClose={handleClose} />
  );
  await userEvent.type(screen.getByLabelText(/Username/), "network-error");
  await userEvent.type(screen.getByLabelText(/Password/), "asdf{enter}");
  await screen.findByText(/network error/);
  await waitFor(() =>
    expect(screen.queryByText(/network error/)).not.toBeInTheDocument()
  );
  expect(onSuccess).toHaveBeenCalledTimes(0);
});
