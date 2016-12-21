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

So far, the web interface is basic: just a table with links to one-hour
segments of video. Although the backend supports generating `.mp4` files for
arbitrary time ranges, you have to construct URLs by hand. There's also no
support for motion detection, no authentication, and no config UI.

This is version 0.1, the initial release. Until version 1.0, there will be no
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
from source.

The `rust` branch contains a rewrite into the [Rust Programming
Language](https://www.rust-lang.org/en-US/). In the long term, I expect this
will result in a more secure, full-featured, easy-to-install software. In the
short term, there will be growing pains. Rust is a new programming language.
Moonfire NVR's primary author is new to Rust. And Moonfire NVR is a young
project.

You will need the following C libraries installed:

* [ffmpeg](http://ffmpeg.org/) version 2.x, including `libavutil`,
  `libavcodec` (to inspect H.264 frames), and `libavformat` (to connect to RTSP
  servers and write `.mp4` files).

  Note ffmpeg 3.x isn't supported yet by the Rust `ffmpeg` crate; see
  [rust-ffmpeg/issues/64](https://github.com/meh/rust-ffmpeg/issues/64).

  Additionally, ffmpeg library versions older than 55.1.101, along with
  55.1.101, along with all versions of the competing project
  [libav](http://libav.org), don't not support socket timeouts for RTSP. For
  reliable reconnections on error, it's strongly recommended to use ffmpeg
  library versions >= 55.1.101.

* [SQLite3](https://www.sqlite.org/).

On Ubuntu 16.04.1 LTS or Raspbian Jessie, the following command will install
all non-Rust dependencies:

    $ sudo apt-get install \
                   build-essential \
                   libavcodec-dev \
                   libavformat-dev \
                   libavutil-dev \
                   sqlite3 \
                   libsqlite3-dev \
                   uuid-runtime

uuid-runtime is only necessary if you wish to use the uuid command to generate
uuids for your cameras (see below). If you obtain them elsewhere, you can skip this
package.

Next, you need Rust and Cargo. The easiest way to install them is by following
the instructions at [rustup.rs](https://www.rustup.rs/). Note that Rust 1.13
has a serious bug on ARM ([see
announcement](https://blog.rust-lang.org/2016/11/10/Rust-1.13.html)); on those
platforms, prefer using Rust 1.14 betas instead.

You can continue to follow the build/install instructions below for a manual
build and install, or alternatively you can run the prep script called `prep.sh`.

    $ cd moonfire-nvr
    $ ./prep.sh

The script will take the following command line options, should you need them:

* `-D`: Skip database initialization.
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

    $ cargo test
    $ cargo build --release
    $ sudo install -m 755 target/release/moonfire-nvr /usr/local/bin

# Further configuration

Moonfire NVR should be run under a dedicated user. It keeps two kinds of
state:

   * a SQLite database, typically <1 GiB. It should be stored on flash if
     available.
   * the "sample file directory", which holds the actual samples/frames of
     H.264 video. This should be quite large and typically is stored on a hard
     drive.

(See [guide/schema.md](guide/schema.md) for more information.)

Both kinds of state are intended to be accessed only by Moonfire NVR itself.
However, the interface for adding new cameras is not yet written, so you will
have to manually create the database and insert cameras with the `sqlite3`
command line tool prior to starting Moonfire NVR.

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
        ...>     main_rtsp_path, sub_rtsp_path, retain_bytes,
        ...>     next_recording_id) values (
        ...>     X'b47f48706d91414591cd6c931bf836b4', 'driveway',
        ...>     'Longer description of this camera', '192.168.1.101',
        ...>     'admin', '12345', '/Streaming/Channels/1',
        ...>     '/Streaming/Channels/2', 104857600, 0);
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
            retain_bytes, next_recording_id
        )
        values
        (
            X'1c944181b8074b8083eb579c8e194451',
            'Front Left', 'Front Left Driveway',
            '192.168.1.41',
            'admin', 'secret',
            '/Streaming/Channels/1', '/Streaming/Channels/2',
            346870912000, 0
        ),
        (
            X'da5921f493ac4279aafe68e69e174026',
            'Front Right', 'Front Right Driveway',
            '192.168.1.42',
            'admin', 'secret',
            '/Streaming/Channels/1', '/Streaming/Channels/2',
            346870912000, 0
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
        --sample-file-dir=/var/lib/moonfire-nvr/sample \
        --db-dir=/var/lib/moonfire-nvr/db \
        --http-addr=0.0.0.0:8080
    Environment=RUST_LOG=info
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

While Moonfire NVR is running, logs will be written to stdout. The `RUST_LOG`
environmental variable controls the log level; `RUST_LOG=info` is recommended.
If running through systemd, try `sudo journalctl --unit moonfire-nvr` to view
the logs.

If Moonfire NVR crashes with a `SIGSEGV`, the problem is likely an
incompatible version of the C `ffmpeg` libraries; use the latest 2.x release
instead. This is one of the Rust growing pains mentioned above. While most
code written in Rust is "safe", the foreign function interface is not only
unsafe but currently error-prone.

# <a name="help"></a> Getting help and getting involved

Please email the
[moonfire-nvr-users]([https://groups.google.com/d/forum/moonfire-nvr-users)
mailing list with questions, bug reports, feature requests, or just to say
you love/hate the software and why.

I'd welcome help with testing, development (in Rust, JavaScript, and HTML),
user interface/graphic design, and documentation. Please email the mailing
list if interested. Patches are welcome, but I encourage you to discuss large
changes on the mailing list first to save effort.

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
