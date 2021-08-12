// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

export interface Part {
  mimeType: string;
  videoSampleEntryId: number;
  body: Uint8Array;
}

interface ParseSuccess {
  status: "success";
  part: Part;
}

interface ParseError {
  status: "error";
  errorMessage: string;
}

const DECODER = new TextDecoder("utf-8");
const CR = "\r".charCodeAt(0);
const NL = "\n".charCodeAt(0);

type ParseResult = ParseSuccess | ParseError;

/// Parses a live stream message.
export function parsePart(raw: Uint8Array): ParseResult {
  // Parse into headers and body.
  const headers = new Headers();
  let pos = 0;
  while (true) {
    const cr = raw.indexOf(CR, pos);
    if (cr === -1 || raw.length === cr + 1 || raw[cr + 1] !== NL) {
      return {
        status: "error",
        errorMessage: "header that never ends (no '\\r\\n')!",
      };
    }
    const line = DECODER.decode(raw.slice(pos, cr));
    pos = cr + 2;
    if (line.length === 0) {
      break;
    }
    const colon = line.indexOf(":");
    if (colon === -1 || line.length === colon + 1 || line[colon + 1] !== " ") {
      return {
        status: "error",
        errorMessage: "invalid name/value separator (no ': ')!",
      };
    }
    const name = line.substring(0, colon);
    const value = line.substring(colon + 2);
    headers.append(name, value);
  }
  const body = raw.slice(pos);

  const mimeType = headers.get("Content-Type");
  if (mimeType === null) {
    return { status: "error", errorMessage: "no Content-Type" };
  }
  const videoSampleEntryIdStr = headers.get("X-Video-Sample-Entry-Id");
  if (videoSampleEntryIdStr === null) {
    return { status: "error", errorMessage: "no X-Video-Sample-Entry-Id" };
  }
  const videoSampleEntryId = parseInt(videoSampleEntryIdStr, 10);
  if (isNaN(videoSampleEntryId)) {
    return { status: "error", errorMessage: "invalid X-Video-Sample-Entry-Id" };
  }
  return {
    status: "success",
    part: { mimeType, videoSampleEntryId, body },
  };
}
