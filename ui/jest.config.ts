// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2023 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import type { Config } from "jest";

const config: Config = {
  testEnvironment: "./FixJSDomEnvironment.ts",

  transform: {
    // https://github.com/swc-project/jest
    "\\.[tj]sx?$": [
      "@swc/jest",
      {
        // https://swc.rs/docs/configuration/compilation
        // https://github.com/swc-project/jest/issues/167#issuecomment-1809868077
        jsc: {
          transform: {
            react: {
              runtime: "automatic",
            },
          },
        },
      },
    ],
  },

  setupFilesAfterEnv: ["<rootDir>/src/setupTests.ts"],

  // https://github.com/jaredLunde/react-hook/issues/300#issuecomment-1845227937
  moduleNameMapper: {
    "@react-hook/(.*)": "<rootDir>/node_modules/@react-hook/$1/dist/main",
  },
};

export default config;
