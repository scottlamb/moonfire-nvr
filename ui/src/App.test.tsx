// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import { screen } from "@testing-library/react";
import App from "./App";
import { renderWithCtx } from "./testutil";
import { rest } from "msw";
import { setupServer } from "msw/node";

const server = setupServer(
  rest.get("/api/", (req, res, ctx) => {
    return res(ctx.status(503), ctx.text("server error"));
  })
);
beforeAll(() => server.listen({ onUnhandledRequest: "error" }));
afterEach(() => server.resetHandlers());
afterAll(() => server.close());

test("instantiate", async () => {
  renderWithCtx(<App />);
  expect(screen.getByText(/Moonfire NVR/)).toBeInTheDocument();
});
