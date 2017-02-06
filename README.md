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



Moonfire NVR is written in the [Rust Programming
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

* [`ncursesw`](https://www.gnu.org/software/ncurses/), the UTF-8 version of
  the `ncurses` library.

On Ubuntu 16.04.1 LTS or Raspbian Jessie, the following command will install
all non-Rust dependencies:

    $ sudo apt-get install \
                   build-essential \
                   libavcodec-dev \
                   libavformat-dev \
                   libavutil-dev \
                   libncursesw-dev \
                   libsqlite3-dev

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
installed moonfire-nvr binary. The only thing
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
have to manually insert cameras with the `sqlite3` command line tool prior to
starting Moonfire NVR.

Manual commands would look something like this:

    $ sudo addgroup --system moonfire-nvr
    $ sudo adduser --system moonfire-nvr --home /var/lib/moonfire-nvr
    $ sudo mkdir /var/lib/moonfire-nvr
    $ sudo -u moonfire-nvr -H mkdir db sample
    $ sudo -u moonfire-nvr moonfire-nvr init

## <a name="cameras"></a>Camera configuration and hard drive mounting

If a dedicated hard drive is available, set up the mount point:

    $ sudo vim /etc/fstab
    $ sudo mount /var/lib/moonfire-nvr/sample

Once setup is complete, it is time to add camera configurations to the
database. If the daemon is running, you will need to stop it temporarily:

    $ sudo systemctl stop moonfire-nvr

You can configure the system through a text-based user interface:

    $ sudo -u moonfire-nvr moonfire-nvr config

In the user interface, add your cameras under the "Edit cameras" dialog.
There's a "Test" button to verify your settings directly from the dialog.

After the cameras look correct, go to "Edit retention" to assign disk space to
each camera. Leave a little slack (at least 100 MB per camera) between the total
limit and the filesystem capacity, even if you store nothing else on the disk.
There are several reasons this is needed:

   * The limit currently controls fully-written files only. There will be up
     to two minutes of video per camera of additional video.
   * The rotation happens after the limit is exceeded, not proactively.
   * Moonfire NVR currently doesn't account for the unused space in the final
     filesystem block at the end of each file.
   * Moonfire NVR doesn't account for the space used for directory listings.
   * If a file is open when it is deleted (such as if a HTTP client is
     downloading it), it stays around until the file is closed. Moonfire NVR
     currently doesn't account for this.

When finished, start the daemon:

    $ sudo systemctl start moonfire-nvr

## System Service

Moonfire NVR can be run as a systemd service. If you used `prep.sh` this has
been done for you. If not, Create
`/etc/systemd/system/moonfire-nvr.service`:

    [Unit]
    Description=Moonfire NVR
    After=network-online.target

    [Service]
    ExecStart=/usr/local/bin/moonfire-nvr run \
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
    $ sudo systemctl start moonfire-nvr
    $ sudo systemctl status moonfire-nvr
    $ sudo systemctl enable moonfire-nvr

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
