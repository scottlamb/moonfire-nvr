// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

/**
 * @file Convenience wrapper around the Moonfire NVR API layer.
 *
 * See <tt>ref/api.md</tt> for a description of the API. Some of the
 * documentation is copied into the docstrings here for convenience, but
 * that doc is authoritative.
 *
 * The functions here return a Typescript discriminating union of status.
 * This seems convenient for ensuring the caller handles all possibilities.
 */

import { Camera, Session, Stream } from "./types";

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
    if (!(e instanceof DOMException)) {
      throw e;
    }
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
      if (!(e instanceof DOMException)) {
        throw e;
      }
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

export interface InitSegmentResponse {
  aspect: [number, number];
  body: ArrayBuffer;
}

/** Fetches an initialization segment. */
export async function init(
  videoSampleEntryId: number,
  init: RequestInit
): Promise<FetchResult<InitSegmentResponse>> {
  const url = `/api/init/${videoSampleEntryId}.mp4`;
  const fetchRes = await myfetch(url, init);
  if (fetchRes.status !== "success") {
    return fetchRes;
  }
  const rawAspect = fetchRes.response.headers.get("X-Aspect");
  const aspect = rawAspect?.split(":").map((x) => parseInt(x, 10));
  if (aspect === undefined) {
    return {
      status: "error",
      message: `invalid/missing X-Aspect: ${rawAspect}`,
    };
  }
  let body;
  try {
    body = await fetchRes.response.arrayBuffer();
  } catch (e) {
    if (!(e instanceof DOMException)) {
      throw e;
    }
    console.warn(`${url}: unable to read body: ${e.message}`);
    return {
      status: "error",
      message: `unable to read body: ${e.message}`,
    };
  }

  return {
    status: "success",
    response: { aspect: aspect as [number, number], body },
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
    if (!(e instanceof DOMException)) {
      throw e;
    }
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

  // This is not part of the wire API; it's synthesized in `toplevel`.
  streams: Map<number, Stream>;

  permissions: Permissions;
  user: ToplevelUser | undefined;
}

export interface Permissions {
  adminUsers?: boolean;
  readCameraConfigs?: boolean;
  updateSignals?: boolean;
  viewVideo?: boolean;
}

export interface ToplevelUser {
  name: string;
  id: number;
  session: Session | undefined;
}

/** Fetches the top-level API data. */
export async function toplevel(init: RequestInit) {
  const resp = await json<ToplevelResponse>("/api/?days=true", init);
  if (resp.status === "success") {
    resp.response.streams = new Map();
    resp.response.cameras.forEach((c) => {
      for (const key in c.streams) {
        const s = c.streams[key as StreamType]!;
        s.camera = c;
        s.streamType = key as StreamType;
        resp.response.streams.set(s.id, s);
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

export interface UsersResponse {
  users: UserWithId[];
}

export interface UserWithId {
  id: number;
  user: UserSubset;
}

export async function users(init: RequestInit) {
  return await json<UsersResponse>(`/api/users/`, init);
}

export interface PostUserRequest {
  csrf?: string;
  user: UserSubset;
}

export interface UpdateUserRequest {
  csrf?: string;
  precondition?: UserSubset;
  update: UserSubset;
}

export interface UserSubset {
  password?: string | null;
  permissions?: Permissions;
  username?: string;
}

/** Creates a user. */
export async function postUser(req: PostUserRequest, init: RequestInit) {
  return await myfetch("/api/users/", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify(req),
    ...init,
  });
}

/** Updates a user. */
export async function updateUser(
  id: number,
  req: UpdateUserRequest,
  init: RequestInit
) {
  return await myfetch(`/api/users/${id}`, {
    method: "PATCH",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify(req),
    ...init,
  });
}

export interface DeleteUserRequest {
  csrf?: string;
}

/** Deletes a user. */
export async function deleteUser(
  id: number,
  req: DeleteUserRequest,
  init: RequestInit
) {
  return await myfetch(`/api/users/${id}`, {
    method: "DELETE",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify(req),
    ...init,
  });
}

/**
 * Represents a range of one or more recordings as in a single array entry of
 * <tt>GET /api/cameras/&lt;uuid>/&lt;stream>/&lt;recordings></tt>.
 */
export interface Recording {
  /** id of the first recording in this range. */
  startId: number;

  /**
   * If present, indicates that recordings <tt>startId, endId</tt> (inclusive)
   * are described here.
   */
  endId?: number;

  /**
   * If this range is not fully committed to the database, the first id that is
   * uncommitted. This is significant because it's possible that after a crash
   * and restart, this id will refer to a completely different recording. That
   * recording will have a different openId.
   */
  firstUncommitted?: number;

  /**
   * If this boolean is true, the recording endId is still being written to.
   * Accesses to this id (such as view.mp4) may retrieve more data than
   * described here if not bounded by duration. Additionally, if startId ==
   * endId, the start time of the recording is "unanchored" and may change in
   * subsequent accesses.
   */
  growing?: boolean;

  /**
   * Each time Moonfire NVR starts in read-write mode, it is assigned an
   * increasing "open id". This field is the open id as of when these
   * recordings were written. This can be used to disambiguate ids referring to
   * uncommitted recordings.
   */
  openId: number;

  /**
   * start time of the given recording, in the wall time scale. Note this
   * may be less than the requested startTime90k if this recording was ongoing
   * at the requested time.
   */
  startTime90k: number;

  /**
   * end time of the given recording, in the wall time scale. Note this may be
   * greater than the requested endTime90k if this recording was ongoing at the
   * requested time.
   */
  endTime90k: number;

  /**
   * a reference to an entry in the videoSampleEntries object.
   */
  videoSampleEntryId: number;

  /**
   * the number of samples (aka frames) of video in this recording.
   */
  videoSamples: number;

  /**
   * the number of bytes of video in this recording.
   */
  sampleFileBytes: number;
}

export interface VideoSampleEntry {
  width: number;
  height: number;
  pixelHSpacing?: number;
  pixelVSpacing?: number;
  aspectWidth: number;
  aspectHeight: number;
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

/**
 * Returns a URL to a <tt>.mp4</tt> of the given recording.
 * If <tt>trimToRange90k</tt> is supplied, the <tt>.mp4</tt> will include
 * only the portion of the recording which overlaps with the given half-open
 * interval.
 */
export function recordingUrl(
  cameraUuid: string,
  stream: StreamType,
  r: Recording,
  timestampTrack: boolean,
  trimToRange90k?: [number, number]
): string {
  let s = `${r.startId}`;
  if (r.endId !== undefined) {
    s += `-${r.endId}`;
  }
  if (r.firstUncommitted !== undefined) {
    s += `@${r.openId}`; // disambiguate.
  }
  let rel = "";
  if (trimToRange90k !== undefined && r.startTime90k < trimToRange90k[0]) {
    rel += trimToRange90k[0] - r.startTime90k;
  }
  rel += "-";
  if (trimToRange90k !== undefined && r.endTime90k > trimToRange90k[1]) {
    rel += trimToRange90k[1] - r.startTime90k;
  } else if (r.growing) {
    // View just the portion described by recording, not anything added later.
    rel += r.endTime90k - r.startTime90k;
  }
  if (rel !== "-") {
    s += "." + rel;
  }
  return withQuery(`/api/cameras/${cameraUuid}/${stream}/view.mp4`, {
    s,
    ts: timestampTrack,
  });
}
