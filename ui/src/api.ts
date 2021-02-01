// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

/**
 * @file Convenience wrapper around the Moonfire NVR API layer.
 *
 * See <tt>design/api.md</tt> for a description of the API.
 *
 * The functions here return a Typescript discriminating union of status.
 * This seems convenient for ensuring the caller handles all possibilities.
 */

import { Camera, Session } from "./types";

interface FetchSuccess<T> {
  status: "success";
  response: T;
}

interface FetchAborted {
  status: "aborted";
}

export interface FetchError {
  status: "error";
  message: string;
  httpStatus?: number;
}

type FetchResult<T> = FetchSuccess<T> | FetchAborted | FetchError;

async function myfetch(
  url: string,
  init: RequestInit
): Promise<FetchResult<Response>> {
  let response;
  try {
    response = await fetch(url, init);
  } catch (e) {
    if (e.name === "AbortError") {
      return { status: "aborted" };
    } else {
      return {
        status: "error",
        message: `network error: ${e.message}`,
      };
    }
  }
  if (!response.ok) {
    let text;
    try {
      text = await response.text();
    } catch (e) {
      console.warn(
        `${url}: ${response.status}: unable to read body: ${e.message}`
      );
      return {
        status: "error",
        httpStatus: response.status,
        message: `unable to read body: ${e.message}`,
      };
    }
    return {
      status: "error",
      httpStatus: response.status,
      message: text,
    };
  }
  console.debug(`${url}: ${response.status}`);
  return {
    status: "success",
    response,
  };
}

/** Fetches an initialization segment. */
export async function init(
  hash: string,
  init: RequestInit
): Promise<FetchResult<ArrayBuffer>> {
  const url = `/api/init/${hash}.mp4`;
  const fetchRes = await myfetch(url, init);
  if (fetchRes.status !== "success") {
    return fetchRes;
  }
  let body;
  try {
    body = await fetchRes.response.arrayBuffer();
  } catch (e) {
    console.warn(`${url}: unable to read body: ${e.message}`);
    return {
      status: "error",
      message: `unable to read body: ${e.message}`,
    };
  }
  return {
    status: "success",
    response: body,
  };
}

async function json<T>(
  url: string,
  init: RequestInit
): Promise<FetchResult<T>> {
  const fetchRes = await myfetch(url, init);
  if (fetchRes.status !== "success") {
    return fetchRes;
  }
  let body;
  try {
    body = await fetchRes.response.json();
  } catch (e) {
    console.warn(`${url}: unable to read body: ${e.message}`);
    return {
      status: "error",
      message: `unable to read body: ${e.message}`,
    };
  }
  return {
    status: "success",
    response: body,
  };
}

export type ToplevelResponse = {
  timeZoneName: string;
  cameras: Camera[];
  session: Session | undefined;
};

/** Fetches the top-level API data. */
export async function toplevel(init: RequestInit) {
  return await json<ToplevelResponse>("/api/", init);
}

export type LoginRequest = {
  username: string;
  password: string;
};

/** Logs in. */
export async function login(req: LoginRequest, init: RequestInit) {
  return await myfetch("/api/login", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify(req),
    ...init,
  });
}

export type LogoutRequest = {
  csrf: string;
};

/** Logs out. */
export async function logout(req: LogoutRequest, init: RequestInit) {
  return await myfetch("/api/logout", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify(req),
    ...init,
  });
}
