# Introduction

Moonfire NVR is an open-source security camera network video recorder, started
by Scott Lamb <slamb@slamb.org>. Currently it is basic: it saves
H.264-over-RTSP streams from IP cameras to disk as .mp4 files and provides a
simple HTTP interface for listing and viewing fixed-length segments of video.
It does not decode, analyze, or re-encode video frames, so it requires little
CPU. It handles six 720p/15fps streams on a [Raspberry Pi
2](https://www.raspberrypi.org/products/raspberry-pi-2-model-b/), using roughly
5% of the machine's total CPU.

This is version 0.1, the initial release. Until version 1.0, there will be no
compatibility guarantees: configuration and storage formats may change from
version to version.

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
  remote/mobile streaming. Disk space these days is quite cheap (with 3 TB
  drives costing about $100), so we can afford to keep many camera-months of
  both streams on disk.
* decoding and analyzing only select "key" video frames (see
  [wikipedia](https://en.wikipedia.org/wiki/Video_compression_picture_types).
* off-loading expensive work to a GPU. Even the Raspberry Pi has a
  surprisingly powerful GPU.
* using [HTTP Live Streaming](https://en.wikipedia.org/wiki/HTTP_Live_Streaming)
  rather than requiring custom browser plug-ins.
* taking advantage of cameras' built-in motion detection. This is
  the most obvious way to reduce motion detection CPU. It's a last resort
  because these cheap cameras' proprietary algorithms are awful compared to
  those described on [changedetection.net](http://changedetection.net).
  Cameras have high false-positive and false-negative rates, are hard to
  experiment with (as opposed to rerunning against saved video files), and
  don't provide any information beyond if motion exceeded the threshold or
  not.

# Downloading

See the [github page](https://github.com/scottlamb/moonfire-nvr) (in case
you're not reading this text there already). You can download the bleeding
edge version from the commandline via git:

    $ git clone https://github.com/scottlamb/moonfire-nvr.git

# Building from source

There are no binary packages of Moonfire NVR available yet, so it must be built
from source. It requires several packages to build:

* [CMake](https://cmake.org/) version 3.1.0 or higher.
* a C++11 compiler, such as [gcc](https://gcc.gnu.org/) 4.7 or higher.
* [ffmpeg](http://ffmpeg.org/), including `libavutil`,
  `libavcodec` (to inspect H.264 frames), and `libavformat` (to connect to RTSP
  servers and write `.mp4` files). Note ffmpeg versions older than 55.1.101,
  along with all versions of the competing project [libav](http://libav.org),
  does not support socket timeouts for RTSP. For reliable reconnections on
  error, it's strongly recommended to use ffmpeg >= 55.1.101.
* [libevent](http://libevent.org/) 2.1, for the built-in HTTP server.
  (This might be replaced with the more full-featured
  [nghttp2](https://github.com/tatsuhiro-t/nghttp2) in the future.)
  Unfortunately, the libevent 2.0 bundled with current Debian releases is
  unsuitable.
* [protocol buffers](https://developers.google.com/protocol-buffers/),
  currently just for the configuration file.
* [gflags](http://gflags.github.io/gflags/), for commandline flag parsing.
* [glog](https://github.com/google/glog), for debug logging.
* [gperftools](https://github.com/gperftools/gperftools), for debugging.
* [googletest](https://github.com/google/googletest), for automated testing.
  This will be automatically downloaded during the build process, so it's
  not necessary to install it beforehand.
* [re2](https://github.com/google/re2), for parsing with regular expressions.
* libuuid from (util-linux)[https://en.wikipedia.org/wiki/Util-linux].
* [SQLite3](https://www.sqlite.org/).

On Ubuntu 15.10 or Raspbian Jessie, the following command will install most
pre-requisites (see also the `Build-Depends` field in `debian/control`):

    $ sudo apt-get install \
                   build-essential \
                   cmake \
                   libprotobuf-dev \
                   libavcodec-dev \
                   libavformat-dev \
                   libavutil-dev \
                   libgflags-dev \
                   libgoogle-glog-dev \
                   libgoogle-perftools-dev \
                   libre2-dev \
                   libsqlite3-dev \
                   pkgconf \
                   protobuf-compiler \
                   uuid-dev

libevent 2.1 will have to be installed from source. In the future, this
dependency may be replaced or support may be added for automatically building
libevent in-tree to avoid the inconvenience.

Once prerequisites are installed, Moonfire NVR can be built as follows:

    $ mkdir build
    $ cd build
    $ cmake ..
    $ make
    $ sudo make install

Alternatively, if you do have a sufficiently new apt-installed libevent
installed, you may be able to prepare a `.deb` package:

    $ sudo apt-get install devscripts dh-systemd
    $ debuild -us -uc

# Configuration

Moonfire NVR expects a configuration file `/etc/moonfire-nvr.conf` (overridable
with the `--config` argument). Currently this file should contain a
text-format `moonfire_nvr.Config` protocol buffer message; see
`src/config.protodevel` which describes the meaning of fields. The general
syntax is as in the example below: `field: value` for simple fields, or
(`field < ... >`) for "message" fields. It supports line-based comments
starting with #.

    base_path: "/var/lib/moonfire_nvr"
    rotate_sec: 600
    http_port: 8080

    camera <
      short_name: "back_west"
      host: "192.168.1.101:554"
      user: "admin"
      password: "12345"
      main_rtsp_path: "/Streaming/Channels/1"
      sub_rtsp_path: "/Streaming/Channels/2"
      retain_bytes: 52428800  # 50 MiB
    >
    camera <
      short_name: "back_east"
      host: "192.168.1.102:554"
      user: "admin"
      password: "12345"
      main_rtsp_path: "/Streaming/Channels/1"
      sub_rtsp_path: "/Streaming/Channels/2"
      retain_bytes: 52428800  # 50 MiB
    >

The example configuration above does the following:

* streams the `main_rtsp_path` from both cameras, reconnecting on errors, and
  writing 10-minute segments of video to subdirectories of
  `/var/lib/surveillance/`. (The `sub_rtsp_path` field is not used yet.)
* deletes old files to stay within the 50 MiB limit for each camera, excluding
  the video file currently being written.
* writes human-readable debug logs to `/tmp/moonfire_nvr.INFO`.
* runs an HTTP server on the port 8080 (try
  [`http://localhost:8080/`](http://localhost:8080/) which allows streaming the
  video. Note: Moonfire NVR does not yet support authentication or SSL, so
  this webserver should not be directly exposed to the Internet.

When configuring Moonfire NVR, it may be helpful to replicate its basic
functionality with the `ffmpeg` commandline tool. The command below is roughly
equivalent to the configuration for `back_west` above.

    $ ffmpeg \
          -i "rtsp://admin:12345@192.168.1.101:554/Streaming/Channels/1" \
          -c copy \
          -map 0:0 \
          -flags:v +global_header \
          -bsf:v dump_extra \
          -f segment \
          -segment_time 600 \
          -use_strftime 1 \
          -segment_format mp4 \
          %Y%m%d%H%M%S-back_west.mp4

# Installation

Moonfire NVR should be run under a dedicated user. This user should own the
`base_path` directory mentioned in the configuration file. Because video is
served through an HTTP interface, there's no need for any other user to access
the files.

    $ sudo addgroup --system moonfire-nvr
    $ sudo adduser --system moonfire-nvr --group moonfire-nvr
    $ sudo mkdir /var/lib/moonfire_nvr
    $ sudo chown moonfire-nvr:moonfire-nvr /var/lib/moonfire_nvr
    $ sudo chmod 700 /var/lib/moonfire_nvr

It can be run as a systemd service. Create
`/etc/systemd/system/moonfire-nvr.service`:

    [Unit]
    Description=Moonfire NVR
    After=network-online.target

    [Service]
    ExecStart=/usr/local/bin/moonfire_nvr
    Type=simple
    User=moonfire-nvr
    Nice=-20
    Restart=on-abnormal
    CPUAccounting=true
    MemoryAccounting=true
    BlockIOAccounting=true

    [Install]
    WantedBy=multi-user.target

Complete the installation through `systemctl` commands:

    $ sudo systemctl daemon-reload
    $ sudo systemctl start moonfire-nvr.service
    $ sudo systemctl status moonfire-nvr.service
    $ sudo systemctl enable moonfire-nvr.service

See the [systemd](http://www.freedesktop.org/wiki/Software/systemd/)
documentation for more information. The [manual
pages](http://www.freedesktop.org/software/systemd/man/) for `systemd.service`
and `systemctl` may be of particular interest.

While Moonfire NVR is running, logs will be written to `/tmp/moonfire_nvr.INFO`.

# <a name="help"></a> Getting help and getting involved

Please email the
[moonfire-nvr-users]([https://groups.google.com/d/forum/moonfire-nvr-users)
mailing list with questions, bug reports, feature requests, or just to say
you love/hate the software and why.

I'd welcome help with testing, development (in C++, JavaScript, and HTML), user
interface/graphic design, and documentation. Please email the mailing list
if interested. Patches are welcome, but I encourage you to discuss large
changes on the mailing list first to save effort.

C++ code should be written using C++11 features, should follow the [Google C++
style guide](https://google.github.io/styleguide/cppguide.html) for
consistency, and should be automatically tested where practical. But don't
worry about this too much; I'm much happier to work with you to refine a rough
draft patch than never see your contribution at all!

# License

This file is part of Moonfire NVR, a security camera digital video recorder.
Copyright (C) 2016 Scott Lamb <slamb@slamb.org>

This program is free software: you can redistribute it and/or modify
it under the terms of the GNU General Public License as published by
the Free Software Foundation, either version 3 of the License, or
(at your option) any later version.

In addition, as a special exception, the copyright holders give
permission to link the code of portions of this program with the
OpenSSL library under certain conditions as described in each
individual source file, and distribute linked combinations including
the two.

You must obey the GNU General Public License in all respects for all
of the code used other than OpenSSL. If you modify file(s) with this
exception, you may extend this exception to your version of the
file(s), but you are not obligated to do so. If you do not wish to do
so, delete this exception statement from your version. If you delete
this exception statement from all source files in the program, then
also delete it here.

This program is distributed in the hope that it will be useful,
but WITHOUT ANY WARRANTY; without even the implied warranty of
MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
GNU General Public License for more details.

You should have received a copy of the GNU General Public License
along with this program.  If not, see <http://www.gnu.org/licenses/>.
