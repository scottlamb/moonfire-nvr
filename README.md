[![CI](https://github.com/scottlamb/moonfire-nvr/workflows/CI/badge.svg)](https://github.com/scottlamb/moonfire-nvr/actions?query=workflow%3ACI)

# Introduction

Moonfire NVR is an open-source security camera network video recorder, started
by Scott Lamb &lt;<slamb@slamb.org>&gt;. It saves H.264-over-RTSP streams from
IP cameras to disk into a hybrid format: video frames in a directory on
spinning disk, other data in a SQLite3 database on flash. It can construct
`.mp4` files for arbitrary time ranges on-the-fly. It does not decode,
analyze, or re-encode video frames, so it requires little CPU. It handles six
1080p/30fps streams on a [Raspberry Pi
2](https://www.raspberrypi.org/products/raspberry-pi-2-model-b/), using
less than 10% of the machine's total CPU.

So far, the web interface is basic: a filterable list of video segments,
with support for trimming them to arbitrary time ranges. No scrub bar yet.
There's also an experimental live view UI.

<table>
  <tbody>
    <tr valign=top>
      <td><a href="screenshots/list.png"><img src="screenshots/list.png" width=360 height=345 alt="list view screenshot"></a></td>
      <td><a href="screenshots/live.jpg"><img src="screenshots/live.jpg" width=360 height=212 alt="live view screenshot"></a></td>
    </tr>
  </tbody>
</table>

There's no support yet for motion detection, no https/SSL/TLS support (you'll
need a proxy server, as described [here](guide/secure.md)), and only a
console-based (rather than web-based) configuration UI.

Moonfire NVR is currently at version 0.6.3. Until version 1.0, there will be no
compatibility guarantees: configuration and storage formats may change from
version to version. There is an [upgrade procedure](guide/schema.md) but it is
not for the faint of heart.

I hope to add features such as salient motion detection. It's way too early to
make promises, but it seems possible to build a full-featured
hobbyist-oriented multi-camera NVR that requires nothing but a cheap machine
with a big hard drive. I welcome help; see [Getting help and getting
involved](#help) below. There are many exciting techniques we could use to
make this possible:

* avoiding CPU-intensive H.264 encoding in favor of simply continuing to use the
  camera's already-encoded video streams. Cheap IP cameras these days provide
  pre-encoded H.264 streams in both "main" (full-sized) and "sub" (lower
  resolution, compression quality, and/or frame rate) varieties. The "sub"
  stream is more suitable for fast computer vision work as well as
  remote/mobile streaming. Disk space these days is quite cheap (with 4 TB
  drives costing about $100), so we can afford to keep many camera-months of
  both streams on disk.
* off-loading on-NVR analytics to an inexpensive USB or M.2 neural network
  accelerator.
* taking advantage of on-camera analytics. This is the lowest CPU usage option,
  although many cameras' analytics aren't as good as what can be done on the NVR,
  they're hard to experiment with, and even when they use modern ML-based
  approaches, their built-in models can't be retrained.

# Documentation

*   [License](LICENSE.txt) —
    [GPL-3.0-or-later](https://spdx.org/licenses/GPL-3.0-or-later.html)
    with [https://spdx.org/licenses/GPL-3.0-linking-exception.html](GPL-3.0-linking-exception)
    for OpenSSL.
*   [Installing](guide/install.md)
*   [Building from source](guide/build.md)
*   [UI Development](guide/developing-ui.md)
*   [Troubleshooting](guide/troubleshooting.md)
*   [Wiki](https://github.com/scottlamb/moonfire-nvr/wiki) has notes on
    several camera models. Please add yours!

# <a name="help"></a> Getting help and getting involved

Please email the
[moonfire-nvr-users](https://groups.google.com/d/forum/moonfire-nvr-users)
mailing list with questions, or just to say you love/hate the software and
why. You can also file bugs and feature requests on the
[github issue tracker](https://github.com/scottlamb/moonfire-nvr/issues).

I'd welcome help with testing, development (in Rust, JavaScript, and HTML),
user interface/graphic design, and documentation. Please email the mailing
list if interested. Pull requests are welcome, but I encourage you to discuss
large changes on the mailing list or in a github issue first to save effort.
