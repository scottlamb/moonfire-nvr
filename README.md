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
edge version from the command line via git:

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
* [gflags](http://gflags.github.io/gflags/), for command line flag parsing.
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
                   libavcodec-dev \
                   libavformat-dev \
                   libavutil-dev \
                   libgflags-dev \
                   libgoogle-glog-dev \
                   libgoogle-perftools-dev \
                   libre2-dev \
                   sqlite3 \
                   libsqlite3-dev \
                   pkgconf \
                   uuid-runtime \
                   uuid-dev

libevent 2.1 will have to be installed from source. In the future, this
dependency may be replaced or support may be added for automatically building
libevent in-tree to avoid the inconvenience.

uuid-runtime is only necessary if you wish to use the uuid command to generate
uuids for your cameras (see below). If you obtain them elsewhere, you can skip this
package.

You can continue to follow the build/install instructions below for a manual
build and install, or alternatively you can run the prep script called `prep.sh`.

    $ cd moonfire-nvr
    $ ./prep.sh

The script will take the following command line options, should you need them:

* `-E`: Forcibly purge all existing libevent packages. You would only do this
        if there is some apparent conflict (see remarks about building libevent
        from source).
* `-f`: Force a build even if the binary appears to be installed. This can be useful
        on repeat builds.
* `-S`: Skip updating and installing dependencies through apt-get. This too can be
        useful on repeated builds.

You can edit variables at the start of the script to influence names and
directories, but defaults should suffice in most cases. For details refer to
the script itself. We will mention just one option, needed when you follow the
suggestion to separate database and samples between flash storage and a hard disk.
If you have the hard disk mounted on, lets say `/media/nvr`, and you want to
store the video samples inside a directory named `samples` there, you would set:

    SAMPLES_DIR=/media/nvr/samples

The script will perform all necessary steps to leave you with a fully built,
installed moonfire-nvr binary and (running) system service. The only thing
you'll have to do manually is add your camera configuration(s) to the database.
Alternatively, before running the script, you can create a file named `cameras.sql`
in the same directory as the `prep.sh` script and it will be automatically
included for you.
For instructions, you can skip to "[Camera configuration and hard disk mounting](#camera)".

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

# Further configuration

Moonfire NVR should be run under a dedicated user. It keeps two kinds of
state:

* a SQLite database, typically <1 GiB. It should be stored on flash if
  available.
* the "sample file directory", which holds the actual samples/frames of H.264
  video. This should be quite large and typically is stored on a hard drive.

Both are intended to be accessed only by Moonfire NVR itself. However, the
interface for adding new cameras is not yet written, so you will have to
manually create the database and insert cameras with the `sqlite3` command line
tool prior to starting Moonfire NVR.

Manual commands would look something like this:

    $ sudo addgroup --system moonfire-nvr
    $ sudo adduser --system moonfire-nvr --home /var/lib/moonfire-nvr
    $ sudo mkdir /var/lib/moonfire-nvr
    $ sudo -u moonfire-nvr -H mkdir db sample
    $ sudo -u moonfire-nvr sqlite3 ~moonfire-nvr/db/db < path/to/schema.sql

## <a name="cameras"></a>Camera configuration and hard drive mounting

If a dedicated hard drive is available, set up the mount point:

    $ sudo vim /etc/fstab
    $ sudo mount /var/lib/moonfire-nvr/sample

Once setup is complete, it is time to add camera configurations to the
database.  However, the interface for adding new cameras is not yet written,
so you will have to manually insert cameras configurations with the `sqlite3`
command line tool prior to starting Moonfire NVR.

Before setting up a camera, it may be helpful to test settings with the
`ffmpeg` command line tool:

    $ ffmpeg \
          -i "rtsp://admin:12345@192.168.1.101:554/Streaming/Channels/1" \
          -c copy \
          -map 0:0 \
          -rtsp_transport tcp \
          -flags:v +global_header \
          test.mp4

Once you have a working `ffmpeg` command line, insert the camera config as
follows.  See the schema SQL file's comments for more information.
Note that the sum of `retain_bytes` for all cameras combined should be
somewhat less than the available bytes on the sample file directory's
filesystem, as the currently-writing sample files are not included in
this sum. Be sure also to subtract out the filesystem's reserve for root
(typically 5%).

In the following example, we generate a uuid which is then later used
to uniquely identify this camera. Thus, you will generate a new one for
each camera you insert using this method.

    $ uuidgen | sed -e 's/-//g'
    b47f48706d91414591cd6c931bf836b4
    $ sudo -u moonfire-nvr sqlite3 ~moonfire-nvr/db/db
    sqlite3> insert into camera (
        ...>     uuid, short_name, description, host, username, password,
        ...>     main_rtsp_path, sub_rtsp_path, retain_bytes) values (
        ...>     X'b47f48706d91414591cd6c931bf836b4', 'driveway',
        ...>     'Longer description of this camera', '192.168.1.101',
        ...>     'admin', '12345', '/Streaming/Channels/1',
        ...>     '/Streaming/Channels/2', 104857600);
    sqlite3> ^D

### Using automatic camera configuration inclusion with `prep.sh`

Not withstanding the instructions above, you can also prepare a file named
`cameras.sql` before you run the `prep.sh` script. The format of this file
should be something like in the example below for two cameras (SQL gives you
lots of freedom in the use of blank space and newlines, so this is formatted
for easy reading, and editing, and does not have to be altered in formatting,
but can if you wish and know what you are doing):

    insert into camera (
    		uuid,
    		short_name, description,
    		host, username, password,
    		main_rtsp_path, sub_rtsp_path,
    		retain_bytes
    	)
    	values
    	(
    		X'1c944181b8074b8083eb579c8e194451',
    		'Front Left', 'Front Left Driveway',
    		'192.168.1.41',
    		'admin', 'secret',
    		'/Streaming/Channels/1', '/Streaming/Channels/2',
    		346870912000
    	),
    	(
    		X'da5921f493ac4279aafe68e69e174026',
    		'Front Right', 'Front Right Driveway',
    		'192.168.1.42',
    		'admin', 'secret',
    		'/Streaming/Channels/1', '/Streaming/Channels/2',
    		346870912000
    	);

You'll still have to find the correct rtsp paths, usernames and passwords, and
set retained byte counts, as explained above.

## System Service

Moonfire NVR can be run as a systemd service. If you used `prep.sh` this has
been done for you. If not, Create
`/etc/systemd/system/moonfire-nvr.service`:

    [Unit]
    Description=Moonfire NVR
    After=network-online.target

    [Service]
    ExecStart=/usr/local/bin/moonfire-nvr \
        --sample_file_dir=/var/lib/moonfire-nvr/sample \
        --db_dir=/var/lib/moonfire-nvr/db \
        --http_port=8080
    Type=simple
    User=moonfire-nvr
    Nice=-20
    Restart=on-abnormal
    CPUAccounting=true
    MemoryAccounting=true
    BlockIOAccounting=true

    [Install]
    WantedBy=multi-user.target

Note that the HTTP port currently has no authentication; it should not be
directly exposed to the Internet.

Complete the installation through `systemctl` commands:

    $ sudo systemctl daemon-reload
    $ sudo systemctl start moonfire-nvr.service
    $ sudo systemctl status moonfire-nvr.service
    $ sudo systemctl enable moonfire-nvr.service

See the [systemd](http://www.freedesktop.org/wiki/Software/systemd/)
documentation for more information. The [manual
pages](http://www.freedesktop.org/software/systemd/man/) for `systemd.service`
and `systemctl` may be of particular interest.

# Troubleshooting

While Moonfire NVR is running, logs will be written to `/tmp/moonfire-nvr.INFO`.
Also available will be `/tmp/moonfire-nvr.WARNING` and `/tmp/moonfire-nvr.ERROR`.
These latter to contain only warning or more serious messages, respectively only
error messages.

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
