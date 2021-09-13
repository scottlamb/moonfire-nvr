# Moonfire NVR Time Handling <!-- omit in toc -->

Status: **current**.

> A man with a watch knows what time it is. A man with two watches is never
> sure.
>
> — Segal's law

* [Objective](#objective)
* [Background](#background)
* [Overview](#overview)
* [Detailed design](#detailed-design)
* [Caveats](#caveats)
    * [Stream mismatches](#stream-mismatches)
    * [Time discontinuities](#time-discontinuities)
    * [Leap seconds](#leap-seconds)
        * [Use `clock_gettime(CLOCK_TAI, ...)` timestamps](#use-clock_gettimeclock_tai--timestamps)
        * [Use a leap second table when calculating differences](#use-a-leap-second-table-when-calculating-differences)
        * [Use smeared time](#use-smeared-time)
* [Alternatives considered](#alternatives-considered)

## Objective

Maximize the likelihood Moonfire NVR's timestamps are useful.

The timestamp corresponding to a video frame should roughly match timestamps
from other sources:

*   another video stream from the same camera. Given a video frame from the
    "main" stream, a video frame from the "sub" stream with a similar
    timestamp should have been recorded near the same time, and vice versa.
    This minimizes confusion when switching between views of these streams,
    and when viewing the "main" stream timestamps corresponding to a motion
    event gathered from the less CPU-intensive "sub" stream.
*  on-camera motion events from the same camera. If the video frame reflects
    the motion event, its timestamp should be roughly within the event's
    timespan.
*   streams from other cameras. Recorded views from two cameras of the same
    event should have similar timestamps.
*   events noted by the owner of the system, neighbors, police, etc., for the
    purpose of determining chronology, to the extent those persons use
    accurate clocks.

Two recordings from the same stream should not overlap. This would make it
impossible for a user interface to present a simple timeline for accessing all
recorded video.

Durations should be useful over short timescales:

*   If an object's motion is recorded, distance travelled divided by the
    duration of the frames over which this motion occurred should reflect the
    object's average speed.
*   Motion should appear smooth. There shouldn't be excessive frame-to-frame
    jitter due to such factors as differences in encoding time or network
    transmission.

This document describes an approach to achieving these goals when the
following statements are true:

*   the NVR's system clock is within a second of correct on startup. (True
    when NTP is functioning or when the system has a real-time clock battery
    to preserve a previous correct time.)
*   the NVR's system time does not experience forward or backward "step"
    corrections (as opposed to frequency correction) during operation.
*   the NVR's system time advances at roughly the correct frequency. (NTP
    achieves this through frequency correction when operating correctly.)
*   the cameras' clock frequencies are off by no more than 500 parts per
    million (roughly 43 seconds per day).
*   the cameras are geographically close to the NVR, so in most cases network
    transmission time is under 50 ms. (Occasional delays are to be expected,
    however.)

When one or more of those statements are false, the system should degrade
gracefully: preserve what properties it can, gather video anyway, and when
possible include sufficient metadata to assess trustworthiness.

Additionally, the system should not require manual configuration of camera
frequency corrections.

## Background

Time in a distributed system is notoriously tricky. [Falsehoods programmers
believe about
time](http://infiniteundo.com/post/25326999628/falsehoods-programmers-believe-about-time)
and [More falsehoods programmers believe about time; "wisdom of the crowd"
edition](http://infiniteundo.com/post/25509354022/more-falsehoods-programmers-believe-about-time)
give a taste of the problems encountered. These problems are found even in
datacenters with expensive, well-tested hardware and relatively reliable
network connections. Moonfire NVR is meant to run on an inexpensive
single-board computer and record video from budget, closed-source cameras,
so such problems are to be expected.

Moonfire NVR typically has access to the following sources of time
information:

*   the local `CLOCK_REALTIME`. Ideally this is maintained by `ntpd`:
    synchronized on startup, and frequency-corrected during operation. A
    hardware real-time clock and battery keep accurate time across restarts
    if the network is unavailable on startup. In the worst case, the system
    has no real-time clock or no battery and a network connection is
    unavailable. The time is far in the past on startup and is never
    corrected or is corrected via a step while Moonfire NVR is running.
*   the local `CLOCK_MONOTONIC`. This should be frequency-corrected by `ntpd`
    and guaranteed to never experience "steps", though its reference point is
    unspecified.
*   the local `ntpd`, which can be used to determine if the system is
    synchronized to NTP and quantify the precision of synchronization.
*   each camera's clock. The ONVIF specification mandates cameras must
    support synchronizing clocks via NTP, but in practice cameras appear to
    use SNTP clients which simply step time periodically and provide no
    interface to determine if the clock is currently synchronized. This
    document's author owns several cameras with clocks that run roughly 20
    *ppm* fast (2 seconds per day) and are adjusted via steps.
*   the RTP timestamps from each of a camera's streams. As described in
    [RFC 3550 section 5.1](https://tools.ietf.org/html/rfc3550#section-5.1),
    these are monotonically increasing with an unspecified reference point.
    They can't be directly compared to other cameras or other streams from
    the same camera. Emperically, budget cameras don't appear to do any
    frequency correction on these timestamps.
*   in some cases, RTCP sender reports, as described in
    [RFC 3550 section 6.4](https://tools.ietf.org/html/rfc3550#section-6.4).
    These correlate RTP timestamps with the camera's real time clock.
    However, these are only sent periodically, not necessarily at the
    beginning of the session.  Some cameras omit them entirely depending on
    firmware version, as noted in
    [this forum post](https://www.cctvforum.com/topic/40914-video-sync-with-hikvision-ipcams-tech-query-about-rtcp/).
    Additionally, Moonfire NVR currently uses ffmpeg's libavformat for RTSP
    protocol handling; this library exposes these reports in a limited
    fashion.

The camera records video frames as in the diagram below:

![Video frame timeline](time-frames.png)

Each frame has an associated RTP timestamp. It's unclear from skimming RFC
3550 exactly what time this represents, but it must be some time after the
last frame and before the next frame. At a typical rate of 30 frames per
second, this timespan is short enough that this uncertainty won't be the
largest source of time error in the system. We'll assume arbitrarily that the
timestamp refers to the start of exposure.

RTP doesn't transmit the duration of each video frame; it must be calculated
from the timestamp of the following frame. This means that if a stream is
terminated, the final frame has unknown duration.

As described in [schema.md](schema.md), Moonfire NVR saves RTSP video streams
into roughly one-minute *recordings,* with a fixed rotation offset after the
minute in the NVR's wall time.

See the [glossary](glossary.md) for additional terminology. Glossary terms
are italicized on first use.

## Overview

Moonfire NVR will use the RTP timestamps to calculate video frames' durations,
relying on the camera's clock for the *media duration* of frames and
recordings. In the first recording in a *run*, it will use these durations
and the NVR's wall clock time to establish the start time of the run. In
subsequent recordings of the run, it will calculate a *wall duration* which
is up to 500 *ppm* different from the media duration to gently correct the
camera's clock toward the NVR's clock, trusting the latter to be more
accurate.

## Detailed design

On every frame of video, Moonfire NVR will get a timestamp from
`CLOCK_MONOTONIC`. On the first frame, it will additionally get a timestamp
from `CLOCK_REALTIME` and compute the difference. It uses these to compute a
monotonically increasing real time of receipt for every frame, called the
_local frame time_. Assuming the local clock is accurate, this time is an
upper bound on when the frame was generated. The difference is the sum of the
following items:

*   H.264 encoding
*   buffering on the camera (particularly when starting the stream—some
    cameras apparently send frames that were captured before the RTSP session
    was established)
*   network transmission time

The _local start time_ of a recording is calculated when ending it. It's
defined as the minimum for all frames of the local frame time minus the
duration of all previous frames. If there are many frames, this means neither
initial buffering nor spikes of delay in H.264 encoding or network
transmission cause the local start time to become inaccurate. The least
delayed frame wins.

The start time of a recording is calculated as follows:

*   For the first recording in a *run*: the start time is the local start
    time.
*   For subsequent recordings: the start time is the end time of the previous
    recording.

The *media duration* of video and audio samples is simply taken from the RTSP
timestamps. For video, this is superior to the local frame time because the
latter is vulnerable to jitter. For audio, this is the only realistic option;
it's infeasible to adjust the duration of audio samples.

The media duration of recordings and runs are simply taken from the media
durations of the samples they contain.

Over a long run, the start time plus the media duration may drift
significantly from the actual time samples were recorded because of
inaccuracies in the camera's clock. Therefore, Moonfire NVR also calculates
a *wall duration* of recordings which more closely matches the NVR's clock.
It is calculated as follows:

*   For the first recording in a run: the wall duration is the media duration.
    At the design limit of 500 *ppm* camera frequency error and an upper
    bound of two minutes duration for the initial recording, this causes
    a maximum of 60 milliseconds of error.
*   For subsequent recordings, the wall duration is the media duration
    adjusted by up to 500 *ppm* to reduce differences between the "local start
    time" and the start time, as follows:
    ```
    limit = media_duration / 2000
    wall_duration = media_duration + clamp(local_start - start, -limit, +limit)
    ```
    Note that for a 1-minute recording, 500 *ppm* is 0.3 ms, or 27 90kHz units.

Each recording's local start time is also stored in the database as a delta to
the recording's start time. These stored values aren't used for normal system
operation but may be handy in understanding and correcting errors.

## Caveats

### Stream mismatches

There's no particular reason to believe this will produce perfectly matched
streams between cameras or even of main and sub streams within a camera.
If this is insufficient, there's an alternate calculation of start time that
could be used in some circumstances: the _camera start time_. The first RTCP
sender report could be used to correlate a RTP timestamp with the camera's
wall clock, and thus calculate the camera's time as of the first frame.

The _start time_ of the first recording could be either its local start time
or its camera start time, determined via the following rules:

1.  if there is no camera start time (due to the lack of a RTCP sender
    report), the local start time wins by default.
2.  if the camera start time is before 2016-01-01 00:00:00 UTC, the local
    start time wins.
3.  if the local start time is before 2016-01-01 00:00:00 UTC, the camera
    start time wins.
4.  if the times differ by more than 5 seconds, the local start time wins.
5.  otherwise, the camera start time wins.

These rules are a compromise. When a system starts up without NTP or a clock
battery, it typically reverts to a time in the distant past. Therefore times
before Moonfire NVR was written should be checked for and avoided. When both
systems have a believably recent timestamp, the local time is typically more
accurate, but the camera time allows a closer match between two streams of
the same camera.

This still doesn't completely solve the problem, and it's unclear it is even
better. When using camera start times, different cameras' streams may be
mismatched by up twice the 5-second threshold described above. This could even
happen for two streams within the same camera if a significant step happens
between their establishment. More frequent SNTP adjustments may help, so that
individual steps are less frequent. Or Moonfire NVR could attempt to address
this with more complexity: use sender reports of established RTSP sessions to
detect and compensate for these clock splits.

It's unclear if these additional mechanisms are desirable or worthwhile. The
simplest approach will be adopted initially and adapted as necessary.

### Time discontinuities

If the local system's wall clock time jumps during a recording ([as has
happened](https://github.com/scottlamb/moonfire-nvr/issues/9#issuecomment-322663674)),
Moonfire NVR will continue to use the initial wall clock time for as long as
the recording lasts. This can result in some unfortunate behaviors:

*   a recording that lasts for months might have an incorrect time all the
    way through because `ntpd` took a few minutes on startup.
*   two recordings that were in fact simultaneous might be recorded with very
    different times because a time jump happened between their starts.

It might be better to use the new time (assuming that ntpd has made a
correction) retroactively. This is unimplemented, but the
`recording_integrity` database table has a `wall_time_delta_90k` field which
could be used for this purpose, either automatically or interactively.

It would also be possible to split a recording in two if a "significant" time
jump is noted, or to allow manually restarting a recording without restarting
the entire program.

### Leap seconds

UTC time is defined as the seconds since epoch _excluding
leap seconds_. Thus, timestamps during the leap second are ambiguous, and
durations across the leap second should be adjusted.

In POSIX, the system clock (as returned by `clock_gettime(CLOCK_REALTIME,
...`) is defined as representing UTC. Note that some
systems may instead be following a [leap
smear](https://developers.google.com/time/smear) policy in which instead of
one second happening twice, the clock runs slower. For a 24-hour period, the
clock runs slower by a factor of 1/86,400 (an extra ~11.6 μs/s).

In Moonfire NVR, all wall times in the database are based on UTC as reported
by the system, and it's assumed that `start + duration = end`. Thus, a leap
second is similar to a one-second time jump (see "Time discontinuities"
above).

Here are some options for improvement:

#### Use `clock_gettime(CLOCK_TAI, ...)` timestamps

Timestamps in the TAI clock system don't skip leap seconds. There's a system
interface intended to provide timestamps in this clock system, and Moonfire
NVR could use it. Unfortunately this has several problems:

*   `CLOCK_TAI` is only available on Linux. It'd be preferable to handle
    timestamps in a consistent way on other platforms. (At least on macOS,
    Moonfire NVR's current primary development platform.)
*   `CLOCK_TAI` is wrong on startup and possibly adjusted later. The offset
    between TAI and UTC is initially assumed to be 0. It's corrected when/if
    a sufficiently new `ntpd` starts.
*   We'd need a leap second table to translate this into calendar time. One
    would have to be downloaded from the Internet periodically, and we'd need
    to consider the case in which the available table is expired.
*   `CLOCK_TAI` likely doesn't work properly with leap smear systems. Where
    the leap smear prevents a time jump for `CLOCK_REALTIME`, it likely
    introduces one for `CLOCK_TAI`.

#### Use a leap second table when calculating differences

Moonfire NVR could retrieve UTC timestamps from the system then translate then
to TAI via a leap second table, either before writing them to the database or
whenever doing math on timestamps.

As with `CLOCK_TAI`, this would require downloading a leap second table from
the Internet periodically.

This would mostly solve the problem at the cost of complexity. Timestamps
obtained from the system for a two-second period starting with each leap
second would still be ambiguous.

#### Use smeared time

Moonfire NVR could make no code changes and ask the system administrator to
use smeared time. This is the simplest option. On a leap smear system, there
are no time jumps. The ~11.6 *ppm* frequency error and the maximum introduced
absolute error of 0.5 sec can be considered acceptable.

Alternatively, Moonfire NVR could assume a specific leap smear policy (such as
24-hour linear smear from 12:00 the day before to 12:00 the day after) and
attempt to correct the time into TAI with a leap second table. This behavior
would work well on a system with the expected configuration and produce
surprising results on other systems. It's unfortunate that there's no standard
way to determine if a system is using a leap smear and with what policy.

## Alternatives considered

Schema versions prior to 6 used a simpler database schema which didn't
distinguish between "wall" and "media" time. Instead, the durations of video
samples were adjusted for clock correction. This approach worked well for
video. It couldn't be extended to audio without decoding and re-encoding to
adjust same lengths and pitch.
