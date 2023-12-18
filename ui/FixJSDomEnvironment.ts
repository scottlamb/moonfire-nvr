// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2023 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

// Environment based on `jsdom` with some extra globals, inspired by
// the following comment:
// https://github.com/jsdom/jsdom/issues/1724#issuecomment-1446858041

import JSDOMEnvironment from "jest-environment-jsdom";

// https://github.com/facebook/jest/blob/v29.4.3/website/versioned_docs/version-29.4/Configuration.md#testenvironment-string
export default class FixJSDOMEnvironment extends JSDOMEnvironment {
  constructor(...args: ConstructorParameters<typeof JSDOMEnvironment>) {
    super(...args);

    // Tests use fetch calls with relative URLs + msw to intercept.
    this.global.fetch = (
      resource: RequestInfo | URL,
      options?: RequestInit
    ) => {
      throw "must use msw to fetch: " + resource;
    };

    class MyRequest extends Request {
      constructor(input: RequestInfo | URL, init?: RequestInit | undefined) {
        input = new URL(input as string, "http://localhost");
        super(input, init);
      }
    }

    this.global.Headers = Headers;
    this.global.Request = MyRequest;
    this.global.Response = Response;

    // `src/LiveCamera/parser.ts` uses TextDecoder.
    this.global.TextDecoder = TextDecoder;
  }
}
