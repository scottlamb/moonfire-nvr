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

export type StreamType = "main" | "sub";

export interface FetchSuccess<T> {
  status: "success";
  response: T;
}

export interface FetchAborted {
  status: "aborted";
}

export interface FetchError {
  status: "error";
  message: string;
  httpStatus?: number;
}

export type FetchResult<T> = FetchSuccess<T> | FetchAborted | FetchError;

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

export interface ToplevelResponse {
  timeZoneName: string;
  cameras: Camera[];
  session: Session | undefined;
}

/** Fetches the top-level API data. */
export async function toplevel(init: RequestInit) {
  const resp = await json<ToplevelResponse>("/api/?days=true", init);
  if (resp.status === "success") {
    resp.response.cameras.forEach((c) => {
      for (const key in c.streams) {
        const s = c.streams[key as StreamType];
        s.camera = c;
        s.streamType = key as StreamType;
      }
    });
  }
  return resp;
}

export interface LoginRequest {
  username: string;
  password: string;
}

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

export interface LogoutRequest {
  csrf: string;
}

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

export interface Recording {
  startId: number;
  endId?: number;
  firstUncommited?: number;
  growing?: boolean;
  openId: number;
  startTime90k: number;
  endTime90k: number;
  videoSampleEntryId: number;
  videoSamples: number;
  sampleFileBytes: number;
}

export interface VideoSampleEntry {
  width: number;
  height: number;
  pixelHSpacing?: number;
  pixelVSpacing?: number;
}

export interface RecordingsRequest {
  cameraUuid: string;
  stream: StreamType;
  startTime90k?: number;
  endTime90k?: number;
  split90k?: number;
}

export interface RecordingsResponse {
  recordings: Recording[];
  videoSampleEntries: { [id: number]: VideoSampleEntry };
}

function withQuery(baseUrl: string, params: { [key: string]: any }): string {
  const p = new URLSearchParams();
  for (const k in params) {
    const v = params[k];
    if (v !== undefined) {
      p.append(k, v.toString());
    }
  }
  const ps = p.toString();
  return ps !== "" ? `${baseUrl}?${ps}` : baseUrl;
}

export async function recordings(req: RecordingsRequest, init: RequestInit) {
  const p = new URLSearchParams();
  if (req.startTime90k !== undefined) {
    p.append("startTime90k", req.startTime90k.toString());
  }
  if (req.endTime90k !== undefined) {
    p.append("endTime90k", req.endTime90k.toString());
  }
  if (req.split90k !== undefined) {
    p.append("split90k", req.split90k.toString());
  }
  const url = withQuery(
    `/api/cameras/${req.cameraUuid}/${req.stream}/recordings`,
    {
      startTime90k: req.startTime90k,
      endTime90k: req.endTime90k,
      split90k: req.split90k,
    }
  );
  return await json<RecordingsResponse>(url, init);
}

export function recordingUrl(
  cameraUuid: string,
  stream: StreamType,
  r: Recording
): string {
  let s = `${r.startId}`;
  if (r.endId !== undefined) {
    s += `-${r.endId}`;
  }
  if (r.firstUncommited !== undefined) {
    s += `@${r.openId}`;
  }
  return withQuery(`/api/cameras/${cameraUuid}/${stream}/view.mp4`, {
    s,
    ts: true,
  });
}
