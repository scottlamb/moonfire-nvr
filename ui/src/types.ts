// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

/**
 * @file Types from the Moonfire NVR API.
 * See descriptions in <tt>design/api.md</tt>.
 */

export type StreamType = "main" | "sub";

export interface Session {
  username: string;
  csrf: string;
}

export interface Camera {
  uuid: string;
  shortName: string;
  description: string;
  streams: Partial<Record<StreamType, Stream>>;
}

export interface Stream {
  camera: Camera; // back-reference added within api.ts.
  streamType: StreamType; // likewise.
  retainBytes: number;
  minStartTime90k: number;
  maxEndTime90k: number;
  totalDuration90k: number;
  totalSampleFileBytes: number;
  fsBytes: number;
  days: Record<string, Day>;
  record: boolean;
}

export interface Day {
  totalDuration90k: number;
  startTime90k: number;
  endTime90k: number;
}
