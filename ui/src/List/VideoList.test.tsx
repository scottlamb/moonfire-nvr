// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import { screen } from "@testing-library/react";
import { utcToZonedTime } from "date-fns-tz";
import format from "date-fns/format";
import { DefaultBodyType, delay, http, HttpResponse, PathParams } from "msw";
import { setupServer } from "msw/node";
import { Recording, VideoSampleEntry } from "../api";
import { renderWithCtx } from "../testutil";
import { Camera, Stream } from "../types";
import VideoList from "./VideoList";
import { beforeAll, afterAll, afterEach, expect, test } from "vitest";

const TEST_CAMERA: Camera = {
  uuid: "c7278ba0-a001-420c-911e-fff4e33f6916",
  shortName: "test-camera",
  description: "",
  streams: {},
};

const TEST_STREAM: Stream = {
  camera: TEST_CAMERA,
  id: 1,
  streamType: "main",
  retainBytes: 0,
  minStartTime90k: 0,
  maxEndTime90k: 0,
  totalDuration90k: 0,
  totalSampleFileBytes: 0,
  fsBytes: 0,
  days: {},
  record: true,
};

const TEST_RANGE1: [number, number] = [
  145747836000000, // 2021-04-26T00:00:00:00000-07:00
  145755612000000, // 2021-04-27T00:00:00:00000-07:00
];

const TEST_RANGE2: [number, number] = [
  145755612000000, // 2021-04-27T00:00:00:00000-07:00
  145763388000000, // 2021-04-28T00:00:00:00000-07:00
];

const TEST_RECORDINGS1: Recording[] = [
  {
    startId: 42,
    openId: 1,
    startTime90k: 145750542570000, // 2021-04-26T08:21:13:00000-07:00
    endTime90k: 145750548150000, // 2021-04-26T08:22:15:00000-07:00
    videoSampleEntryId: 4,
    videoSamples: 1860,
    sampleFileBytes: 248000,
  },
];

const TEST_RECORDINGS2: Recording[] = [
  {
    startId: 42,
    openId: 1,
    startTime90k: 145757651670000, // 2021-04-27T06:17:43:00000-07:00
    endTime90k: 145757656980000, // 2021-04-27T06:18:42:00000-07:00
    videoSampleEntryId: 4,
    videoSamples: 1860,
    sampleFileBytes: 248000,
  },
];

const TEST_VIDEO_SAMPLE_ENTRIES: { [id: number]: VideoSampleEntry } = {
  4: {
    width: 1920,
    height: 1080,
    aspectWidth: 16,
    aspectHeight: 9,
  },
};

function TestFormat(time90k: number) {
  return format(
    utcToZonedTime(new Date(time90k / 90), "America/Los_Angeles"),
    "d MMM yyyy HH:mm:ss"
  );
}

const server = setupServer(
  http.get<PathParams, DefaultBodyType, any>(
    "/api/cameras/:camera/:streamType/recordings",
    async ({ request }) => {
      const url = new URL(request.url);
      const p = url.searchParams;
      const range90k = [
        parseInt(p.get("startTime90k")!, 10),
        parseInt(p.get("endTime90k")!, 10),
      ];
      if (range90k[0] === 42) {
        return HttpResponse.text("server error", { status: 503 });
      }
      if (range90k[0] === TEST_RANGE1[0]) {
        return HttpResponse.json({
          recordings: TEST_RECORDINGS1,
          videoSampleEntries: TEST_VIDEO_SAMPLE_ENTRIES,
        });
      } else {
        await delay(2000); // 2 second delay
        return HttpResponse.json({
          recordings: TEST_RECORDINGS2,
          videoSampleEntries: TEST_VIDEO_SAMPLE_ENTRIES,
        });
      }
    }
  )
);
beforeAll(() => server.listen({ onUnhandledRequest: "error" }));
afterEach(() => server.resetHandlers());
afterAll(() => server.close());

test("load", async () => {
  renderWithCtx(
    <table>
      <VideoList
        stream={TEST_STREAM}
        range90k={TEST_RANGE1}
        setActiveRecording={() => {}}
        formatTime={TestFormat}
        trimStartAndEnd={false}
      />
    </table>
  );
  expect(await screen.findByText(/26 Apr 2021 08:21:13/)).toBeInTheDocument();
});

// This test may be slightly flaky because it uses real timers. It looks like
// msw specifically avoids using test timers:
// https://github.com/mswjs/msw/pull/243
test("slow replace", async () => {
  const { rerender } = renderWithCtx(
    <table>
      <VideoList
        stream={TEST_STREAM}
        range90k={TEST_RANGE1}
        setActiveRecording={() => {}}
        formatTime={TestFormat}
        trimStartAndEnd={false}
      />
    </table>
  );
  expect(await screen.findByText(/26 Apr 2021 08:21:13/)).toBeInTheDocument();
  rerender(
    <table>
      <VideoList
        stream={TEST_STREAM}
        range90k={TEST_RANGE2}
        setActiveRecording={() => {}}
        formatTime={TestFormat}
        trimStartAndEnd={false}
      />
    </table>
  );

  // The first results don't go away immediately.
  expect(screen.getByText(/26 Apr 2021 08:21:13/)).toBeInTheDocument();

  // A loading indicator should show up after a second.
  // The default timeout is 1 second; extend it to pass reliably.
  expect(
    await screen.findByRole("progressbar", {}, { timeout: 2000 })
  ).toBeInTheDocument();

  // Then the second query result should show up.
  expect(
    await screen.findByText(/27 Apr 2021 06:17:43/, {}, { timeout: 2000 })
  ).toBeInTheDocument();
});

test("error", async () => {
  renderWithCtx(
    <table>
      <VideoList
        stream={TEST_STREAM}
        range90k={[42, 64]}
        setActiveRecording={() => {}}
        formatTime={TestFormat}
        trimStartAndEnd={false}
      />
    </table>
  );
  expect(await screen.findByRole("alert")).toHaveTextContent(/server error/);
});
