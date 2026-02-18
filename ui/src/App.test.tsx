// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import { screen } from "@testing-library/react";
import App from "./App";
import { renderWithCtx } from "./testutil";
import { http, HttpResponse } from "msw";
import { setupServer } from "msw/node";
import { beforeAll, afterAll, afterEach, expect, test } from "vitest";

const server = setupServer(
  http.get("/api/", () => {
    return HttpResponse.text("server error", { status: 503 });
  })
);
beforeAll(() => server.listen({ onUnhandledRequest: "error" }));
afterEach(() => server.resetHandlers());
afterAll(() => server.close());

test("instantiate", async () => {
  renderWithCtx(<App />);
  expect(screen.getByText(/Moonfire NVR/)).toBeInTheDocument();
  // Wait for the /api/ fetch to complete and error state to render,
  // so cleanup's abort() doesn't race with msw's response.
  await screen.findByText(/Error querying server/);
});
