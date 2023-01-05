# Moonfire NVR Signals

Status: **draft**.

"Signals" are what Moonfire NVR uses to describe non-video timeseries data
such as "was motion detected?" or "what mode was my burglar alarm in?" They are
intended to be displayed in the UI with the video scrub bar to aid in finding
a relevant portion of video.

## Objective

Goals:

*   represent simple results of on-camera and on-NVR motion detection, e.g.:
    `true`, `false`, or `unknown`.
*   represent external signals such as burglar alarm state, e.g.:
    `off`, `stay`, `away`, `alarm`, or `unknown`.

Non-goals:

*   provide meaningful data when the NVR has inaccurate system time.
*   support internal state necessary for on-NVR motion detection. (This will
    be considered separately.)
*   support fine-grained outputs such as "what are the bounding boxes of all
    detected faces?", "what cells have motion?", audio volume, or audio
    spectograms.

## Overview

hmm, two ideas:

*   just use timestamps everywhere. allow adding/updating historical data.
*   only allow updating the current open. initially, just support setting
    current time. then support extending from a previous request. no ability
    to fill in while NVR is down.
