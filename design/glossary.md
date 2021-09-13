# Moonfire NVR Glossary

*media duration:* the total duration of the actual samples in a recording. These
durations are based on the camera's clock. Camera clocks can be quite
inaccurate, so this may not match the *wall duration*. See [time.md](time.md)
for details.

*open id:* a sequence number representing a time the database was opened in
write mode. One reason for using open ids is to disambiguate unflushed
recordings. Recordings' ids are assigned immediately, without any kind of
database transaction or reservation. Thus if a recording is never flushed
successfully, a following *open* may assign the same id to a new recording.
The open id disambiguates this and should be used whenever referring to a
recording that may be unflushed.

*ppm:* Part Per Million.  Crystal Clock accuracy is defined in terms of ppm or 
parts per million and it gives a convenient way of comparing accuracies 
of different crystal specifications. "A typical crystal has an error of 
100ppm (ish) this translates as 100/1e6 or (1e-4)...So the total error on a day 
is 86400 x 1e-4= 8.64 seconds per day. In a month you would loose 
30x8.64 = 259 seconds or 4.32 minutes per month." 
Source: https://www.best-microcontroller-projects.com/ppm.html

*recording:* the video from a (typically 1-minute) portion of an RTSP session.
RTSP sessions are divided into recordings as a detail of the
storage schema. See [schema.md](schema.md) for details. This concept is exposed
to the frontend code through the API; see [api.md](api.md). It's not exposed in
the user interface; videos are reconstructed from segments automatically.

*run:* all the recordings from a single RTSP session. These are all from the
same *stream* and could be reassembled into a single video with no gaps. If the
 camera is lost and re-established, one run ends and another starts.

*sample:* data associated with a single timestamp within a recording, e.g. a video
frame or a set of 

*sample file:* a file on disk that holds all the samples from a single recording.

*sample file directory:* a directory in the local filesystem that holds all
sample files for one or more streams. Typically there is one directory per disk.

*segment:* part or all of a recording. An API request might ask for a video of
recordings 1–4 starting 80 seconds in. If each recording is exactly 60 seconds,
this would correspond to three segments: recording 2 from 20 seconds in to
the end, all of recording 3, and all of recording 4. See [api.md](api.md).

*session:* a set of authenticated Moonfire NVR requests defined by the use of a
given credential (`s` cookie). Each user may have many credentials and thus
many sessions. Note that in Moonfire NVR's the term "session" by itself has
nothing to do with RTSP sessions; those more closely match a *run*.

*signal:* a timeseries with an enum value. Signals might represent a camera's
motion detection or day/night status. They could also represent an external
input such as a burglar alarm system's zone status. See [api.md](api.md).
Note signals are still under development and not yet exposed in Moonfire NVR's
UI. See [#28](https://github.com/scottlamb/moonfire-nvr/issues/28) for more
information.

*stream:* the "main" or "sub" stream from a given camera. Moonfire NVR expects
cameras support configuring and simultaneously viewing two streams encoded from
the same underlying video and audio source. The difference between the two is
that the "main" stream's video is typically higher quality in terms of frame
rate, resolution, and bitrate. Likewise it may have higher quality audio.
A stream corresponds to an ONVIF "media profile". Each stream has a distinct
RTSP URL that yields a difference RTSP "presentation".

*track:* one of the video, audio, or subtitles associated with a single
*stream*. This is consistent with the definition in ISO/IEC 14496-12 section
3.1.19. Note that RTSP RFC 2326 uses the word "stream" in the same way
Moonfire NVR uses the word "track".

*wall duration:* the total duration of a recording for the purpose of matching
with the NVR's wall clock time. This may not match the same recording's media
duration. See [time.md](time.md) for details.
