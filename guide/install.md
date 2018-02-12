# Installing Moonfire NVR

This document describes how to install Moonfire NVR on a Linux system.

## Downloading

See the [github page](https://github.com/scottlamb/moonfire-nvr) (in case
you're not reading this text there already). You can download the bleeding
edge version from the command line via git:

    $ git clone https://github.com/scottlamb/moonfire-nvr.git

## Building from source

There are no binary packages of Moonfire NVR available yet, so it must be built
from source.

Moonfire NVR is written in the [Rust Programming
Language](https://www.rust-lang.org/en-US/). In the long term, I expect this
will result in a more secure, full-featured, easy-to-install software.

You will need the following C libraries installed:

* [ffmpeg](http://ffmpeg.org/) version 2.x or 3.x, including `libavutil`,
  `libavcodec` (to inspect H.264 frames), and `libavformat` (to connect to RTSP
  servers and write `.mp4` files).

  Note ffmpeg library versions older than 55.1.101, along with all versions of
  the competing project [libav](http://libav.org), don't not support socket
  timeouts for RTSP. For reliable reconnections on error, it's strongly
  recommended to use ffmpeg library versions >= 55.1.101.

* [SQLite3](https://www.sqlite.org/).

* [`ncursesw`](https://www.gnu.org/software/ncurses/), the UTF-8 version of
  the `ncurses` library.

On recent Ubuntu or Raspbian, the following command will install
all non-Rust dependencies:

    $ sudo apt-get install \
                   build-essential \
                   libavcodec-dev \
                   libavformat-dev \
                   libavutil-dev \
                   libncurses5-dev \
                   libncursesw5-dev \
                   libsqlite3-dev \
                   libssl-dev \
                   pkgconf

Next, you need Rust 1.17+ and Cargo. The easiest way to install them is by following
the instructions at [rustup.rs](https://www.rustup.rs/).

Finally, building the UI requires [yarn](https://yarnpkg.com/en/).

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

    $ yarn
    $ yarn build
    $ cargo test
    $ cargo build --release
    $ sudo install -m 755 target/release/moonfire-nvr /usr/local/bin
    $ sudo mkdir /usr/local/lib/moonfire-nvr
    $ sudo cp -R ui-dist /usr/local/lib/moonfire-nvr/ui

## Further configuration

Moonfire NVR should be run under a dedicated user. It keeps two kinds of
state:

   * a SQLite database, typically <1 GiB. It should be stored on flash if
     available.
   * the "sample file directories", which hold the actual samples/frames of
     H.264 video. These should be quite large and are typically stored on hard
     drives.

(See [schema.md](schema.md) for more information.)

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

### <a name="cameras"></a>Camera configuration and hard drive mounting

If a dedicated hard drive is available, set up the mount point:

    $ sudo vim /etc/fstab
    $ sudo mount /var/lib/moonfire-nvr/sample

Once setup is complete, it is time to add camera configurations to the
database. If the daemon is running, you will need to stop it temporarily:

    $ sudo systemctl stop moonfire-nvr

You can configure the system through a text-based user interface:

    $ sudo -u moonfire-nvr moonfire-nvr config 2>debug-log

In the user interface,

 1. add your sample file dirs under "Edit cameras and retention"
 2. add cameras under the "Edit cameras and streams" dialog.
    There's a "Test" button to verify your settings directly from the dialog.
    Be sure to assign each stream you want to capture to a sample file
    directory.
 3. Assign disk space to your cameras back in "Edit cameras and retention".
    Leave a little slack (at least 100 MB per camera) between the total limit
    and the filesystem capacity, even if you store nothing else on the disk.
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

### System Service

Moonfire NVR can be run as a systemd service. If you used `prep.sh` this has
been done for you. If not, Create
`/etc/systemd/system/moonfire-nvr.service`:

    [Unit]
    Description=Moonfire NVR
    After=network-online.target

    [Service]
    ExecStart=/usr/local/bin/moonfire-nvr run \
        --db-dir=/var/lib/moonfire-nvr/db \
        --http-addr=0.0.0.0:8080
    Environment=TZ=:/etc/localtime
    Environment=MOONFIRE_FORMAT=google-systemd
    Environment=MOONFIRE_LOG=info
    Environment=RUST_BACKTRACE=1
    Type=simple
    User=moonfire-nvr
    Nice=-20
    Restart=on-abnormal
    CPUAccounting=true
    MemoryAccounting=true
    BlockIOAccounting=true

    [Install]
    WantedBy=multi-user.target

Note that the HTTP port currently has no authentication, encryption, or
logging; it should not be directly exposed to the Internet.

Complete the installation through `systemctl` commands:

    $ sudo systemctl daemon-reload
    $ sudo systemctl start moonfire-nvr
    $ sudo systemctl status moonfire-nvr
    $ sudo systemctl enable moonfire-nvr

See the [systemd](http://www.freedesktop.org/wiki/Software/systemd/)
documentation for more information. The [manual
pages](http://www.freedesktop.org/software/systemd/man/) for `systemd.service`
and `systemctl` may be of particular interest.
