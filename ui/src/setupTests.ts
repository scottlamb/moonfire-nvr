// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

// jest-dom adds custom jest matchers for asserting on DOM nodes.
// allows you to do things like:
// expect(element).toHaveTextContent(/react/i)
// learn more: https://github.com/testing-library/jest-dom
import "@testing-library/jest-dom";
import { TextDecoder } from "util";

// LiveCamera/parser.ts uses TextDecoder, which works fine from the browser
// but isn't available from node.js without a little help.
// https://create-react-app.dev/docs/running-tests/#initializing-test-environment
// https://stackoverflow.com/questions/51090515/global-functions-in-typescript-for-jest-testing#comment89270564_51091150
declare let global: any;

// TODO: There's likely an elegant way to add TextDecoder to global's type.
// Some promising links:
// https://www.typescriptlang.org/docs/handbook/declaration-merging.html#global-augmentation
// https://stackoverflow.com/a/62011156/23584
// https://github.com/facebook/create-react-app/issues/6553#issuecomment-475491096

global.TextDecoder = TextDecoder;
