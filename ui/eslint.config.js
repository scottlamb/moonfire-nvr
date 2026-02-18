// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2026 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import { defineConfig } from "eslint/config";
import js from "@eslint/js";
import tseslint from "typescript-eslint";
import react from "eslint-plugin-react";
import reactHooks from "eslint-plugin-react-hooks";
import reactRefresh from "eslint-plugin-react-refresh";
import vitest from "@vitest/eslint-plugin";

export default defineConfig(
  { ignores: ["dist/"] },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  react.configs.flat.recommended,
  react.configs.flat["jsx-runtime"],
  reactHooks.configs["recommended-latest"],
  {
    plugins: {
      "react-refresh": reactRefresh,
    },
  },
  {
    files: ["**/*.test.{ts,tsx}"],
    ...vitest.configs.recommended,
  },
  {
    settings: {
      react: {
        version: "detect",
      },
    },
    rules: {
      "no-restricted-imports": [
        "error",
        {
          name: "@mui/material",
          message:
            "Please use deep imports like 'import Button from \"@mui/material/Button\"' to minimize bundle size.",
        },
        {
          name: "@mui/icons-material",
          message:
            "Please use deep imports like 'import MenuIcon from \"@mui/icons-material/Menu\"' to minimize bundle size.",
        },
      ],
      "no-unused-vars": "off",
      "@typescript-eslint/no-unused-vars": ["error", { args: "none" }],
      "@typescript-eslint/no-explicit-any": "off",
      "@typescript-eslint/no-empty-object-type": "off",
      "@typescript-eslint/no-this-alias": "off",
      "react/no-unescaped-entities": "off",
    },
  },
);
