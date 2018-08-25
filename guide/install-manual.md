# Building and installing Moonfire NVR manually

This guide will walk you through building and installing Moonfire NVR manually.
You should have already downloaded the source code as mentioned in
[install.md](install.md), and after completing these instructions you should go
back to that page to complete configuration.

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
  the competing project [libav](http://libav.org), don't support socket
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

Next, you need Rust 1.21+ and Cargo. The easiest way to install them is by
following the instructions at [rustup.rs](https://www.rustup.rs/).

Finally, building the UI requires [yarn](https://yarnpkg.com/en/).

Once prerequisites are installed, Moonfire NVR can be built as follows:

    $ yarn
    $ yarn build
    $ cargo test
    $ cargo build --release
    $ sudo install -m 755 target/release/moonfire-nvr /usr/local/bin
    $ sudo mkdir /usr/local/lib/moonfire-nvr
    $ sudo cp -R ui-dist /usr/local/lib/moonfire-nvr/ui

## Creating the user and database

You can create Moonfire NVR's dedicated user and SQLite database with the
following commands:

    $ sudo addgroup --system moonfire-nvr
    $ sudo adduser --system moonfire-nvr --home /var/lib/moonfire-nvr
    $ sudo mkdir /var/lib/moonfire-nvr
    $ sudo chown moonfire-nvr:moonfire-nvr /var/lib/moonfire-nvr
    $ sudo -u moonfire-nvr -H mkdir db sample
    $ sudo -u moonfire-nvr moonfire-nvr init

## System Service

Moonfire NVR can be run as a systemd service. Create
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

Tell `systemd` to look for the new file:

    $ sudo systemctl daemon-reload

See the [systemd](http://www.freedesktop.org/wiki/Software/systemd/)
documentation for more information. The [manual
pages](http://www.freedesktop.org/software/systemd/man/) for `systemd.service`
and `systemctl` may be of particular interest.

Don't enable or start the service just yet; you'll need to do some more
configuration first.

## Completing installation

After the steps on this page, go back to [Downloading, installing, and
configuring Moonfire NVR](install.md) to set up the sample file directory and
configure the system.
