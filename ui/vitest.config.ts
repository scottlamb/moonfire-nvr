// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2023 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/setupTests.ts"],

    // This avoids node's native fetch from causing vitest workers to hang
    // and use 100% CPU.
    // <https://github.com/vitest-dev/vitest/issues/3077#issuecomment-1815767839>
    pool: "forks",
  },
});
