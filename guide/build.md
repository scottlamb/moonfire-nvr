# Building Moonfire NVR <!-- omit in toc -->

This document has notes for software developers on building Moonfire NVR from
source code for development. If you just want to install precompiled
binaries, see the [Docker installation instructions](install.md) instead.

This document doesn't spell out as many details as the installation
instructions. Please ask on Moonfire NVR's [issue
tracker](https://github.com/scottlamb/moonfire-nvr/issues) or
[mailing list](https://groups.google.com/d/forum/moonfire-nvr-users) when
stuck. Please also send pull requests to improve this doc.

* [Downloading](#downloading)
* [Docker builds](#docker-builds)
    * [Release procedure](#release-procedure)
* [Non-Docker setup](#non-docker-setup)
    * [Running interactively straight from the working copy](#running-interactively-straight-from-the-working-copy)
    * [Running as a `systemd` service](#running-as-a-systemd-service)

## Downloading

See the [github page](https://github.com/scottlamb/moonfire-nvr) (in case
you're not reading this text there already). You can download the
bleeding-edge version from the commandline via git:

```
$ git clone https://github.com/scottlamb/moonfire-nvr.git
$ cd moonfire-nvr
```

## Docker builds

*Note about ARM:* this build procedure is normally used on x86-64 running
Docker 20.10. The author has *cross-compiled to* ARM machines but never
successfully *built on* an ARM machine. In general, the Docker experience on ARM
appears much less polished. For example, you're likely to hit
[this `At least one invalid signature was encountered.` error](https://stackoverflow.com/questions/64439278/gpg-invalid-signature-error-while-running-apt-update-inside-arm32v7-ubuntu20-04).

This command should prepare a deployment image for your local machine:

```
$ docker buildx build --load --tag=moonfire-nvr -f docker/Dockerfile .
```

If you want to iterate on code changes, doing a full Docker build from
scratch every time will be painfully slow. You will likely find it more
helpful to use the `dev` target. This is a self-contained developer environment
which you can use from its shell via `docker run` or via something like
Visual Studio Code's Docker plugin.

```
$ docker buildx build \
        --load --tag=moonfire-dev --target=dev -f docker/Dockerfile .
...
$ docker run \
        --rm --interactive=true --tty \
        --mount=type=bind,source=$(pwd),destination=/var/lib/moonfire-nvr/src \
        moonfire-dev
```

The development image overrides cargo's output directory to
`/var/lib/moonfire-nvr/target`. (See `~moonfire-nvr/.buildrc`.) This avoids
using a bind filesystem for build products, which can be slow on macOS. It
also means that if you sometimes compile directly on the host and sometimes
within Docker, they don't trip over each other's target directories.
directories.

You can also cross-compile to a different architecture. Adding a
`--platform=linux/arm64/v8,linux/arm/v7,linux/amd64` argument will compile
Moonfire NVR for all supported platforms. For the `dev` target, this prepares
a build which executes on your local architecture and is capable of building
a binary for your desired target architecture.

On the author's macOS machine with Docker desktop 3.0.4, building for
multiple platforms at once will initially fail with the following error:

```
$ docker buildx build ... --platform=linux/arm64/v8,linux/arm/v7,linux/amd64
[+] Building 0.0s (0/0)
error: multiple platforms feature is currently not supported for docker driver. Please switch to a different driver (eg. "docker buildx create --use")
```

Running `docker buildx create --use` once solves this problem, with a couple
caveats:

*   you'll need to specify an additional `--load` argument to make builds
    available to run locally.
*   the `--load` argument only works for one platform at a time. With multiple
    platforms, it gives an error like the following:
    ```
    error: failed to solve: rpc error: code = Unknown desc = docker exporter does not currently support exporting manifest lists
    ```
    [A comment on docker/buildx issue
    #59](https://github.com/docker/buildx/issues/59#issuecomment-667548900)
    suggests a workaround of building all three then using caching to quickly
    load the one of immediate interest:
    ```
    $ docker buildx build --platform=linux/arm64/v8,linux/arm/v7,linux/amd64 ...
    $ docker buildx build --load --platform=arm64/v8 ...
    ```

On Linux hosts (as opposed to when using Docker Desktop on macOS/Windows),
you'll likely see errors like the ones below. The solution is to [install
emulators](https://github.com/tonistiigi/binfmt#installing-emulators).

```
Error while loading /usr/sbin/dpkg-split: No such file or directory
Error while loading /usr/sbin/dpkg-deb: No such file or directory
```

Moonfire NVR's `Dockerfile` has some built-in debugging tools:

*   Each stage saves some debug info to `/docker-build-debug/<stage>`, and
    the `deploy` stage preserves the output from previous stages. The debug
    info includes:
    *    output (stdout + stderr) from the build script, running long operations
         through the `time` command.
    *    `ls -laFR` of cache mounts before and after.
*   Each stage accepts a `INVALIDATE_CACHE_<stage>` argument. You can use eg
    `--build-arg=INVALIDATE_CACHE_BUILD_SERVER=$(date +%s)` to force the
    `build-server` stage to be rebuilt rather than use cached Docker layers.

### Release procedure

Releases are currently a bit manual. From a completely clean git work tree,

1.  manually verify the current commit is pushed to github's master branch and
    has a green checkmark indicating CI passed.
2.  update versions:
    *   update `server/Cargo.toml` version by hand; run `cargo test --workspace`
        to update `Cargo.lock`.
    *   ensure `README.md` and `CHANGELOG.md` refer to the new version.
3.  run commands:
    ```bash
    VERSION=x.y.z
    git commit -am "prepare version ${VERSION}"
    git tag -a "v${VERSION}" -m "version ${VERSION}"
    ./release.bash
    git push
    git push origin "v${VERSION}"
    ```

The `release.bash` script needs [`jq`](https://stedolan.github.io/jq/)
installed to work.

## Non-Docker setup

You may prefer building without Docker on the host. Moonfire NVR should run
natively on any Unix-like system. It's been tested on Linux and macOS.
(In theory [Windows Subsystem for
Linux](https://docs.microsoft.com/en-us/windows/wsl/about) should also work.
Please speak up if you try it.)

On macOS systems native builds may be noticeably faster than using Docker's
Linux VM and filesystem overlay.

To build the server, you will need the following C libraries installed:

*   [ffmpeg](http://ffmpeg.org/) version 2.x or 3.x, including `libavutil`,
    `libavcodec` (to inspect H.264 frames), and `libavformat` (to connect to
    RTSP servers and write `.mp4` files).

    Note ffmpeg library versions older than 55.1.101, along with all versions of
    the competing project [libav](http://libav.org), don't support socket
    timeouts for RTSP. For reliable reconnections on error, it's strongly
    recommended to use ffmpeg library versions >= 55.1.101.

*   [SQLite3](https://www.sqlite.org/).

*   [`ncursesw`](https://www.gnu.org/software/ncurses/), the UTF-8 version of
    the `ncurses` library.

To build the UI, you'll need [node and npm](https://nodejs.org/en/download/).

On recent Ubuntu or Raspbian Linux, the following command will install
all non-Rust dependencies:

```
$ sudo apt-get install \
               build-essential \
               libavcodec-dev \
               libavformat-dev \
               libavutil-dev \
               libncurses-dev \
               libsqlite3-dev \
               npm \
               pkgconf \
               sqlite3 \
               tzdata
```

On macOS with [Homebrew](https://brew.sh/) and Xcode installed, try the
following command:

```
$ brew install ffmpeg node
```

Next, you need Rust 1.52+ and Cargo. The easiest way to install them is by
following the instructions at [rustup.rs](https://www.rustup.rs/).

Once prerequisites are installed, you can build the server and find it in
`target/release/moonfire-nvr`:

```
$ cd server
$ cargo test
$ cargo build --release
```

You can build the UI via `npm` and find it in the `ui/dist` directory:

```
$ cd ui
$ npm install
$ npm run build
```

### Running interactively straight from the working copy

The author finds it convenient for local development to set up symlinks so that
the binaries in the working copy will run via just `nvr`:

```
$ sudo mkdir /usr/local/moonfire-nvr
$ sudo ln -s `pwd`/ui-dist /usr/local/moonfire-nvr/ui
$ sudo mkdir /var/lib/moonfire-nvr
$ sudo chown $USER:$USER /var/lib/moonfire-nvr
$ ln -s `pwd`/target/release/moonfire-nvr $HOME/bin/moonfire-nvr 
$ ln -s moonfire-nvr $HOME/bin/nvr
$ nvr init
$ nvr config
$ nvr run
```

(Alternatively, you could symlink to `target/debug/moonfire-nvr` and compile
with `cargo build` rather than `cargo build --release`, for a faster build
cycle and slower performance.)

Note this `nvr` is a little different than the `nvr` shell script you create
when following the [install instructions](install.md). With that shell wrapper,
`nvr run` will create and run a detached Docker container with some extra
arguments specified in the script. This `nvr run` will directly run from the
terminal, with no extra arguments, until you abort with Ctrl-C. Likewise,
some of the shell script's subcommands that wrap Docker (`start`, `stop`, and
`logs`) have no parallel with this `nvr`.

### Running as a `systemd` service

If you want to deploy a non-Docker build on Linux, you may want to use
`systemd`. Create `/etc/systemd/system/moonfire-nvr.service`:

```
[Unit]
Description=Moonfire NVR
After=network-online.target

[Service]
ExecStart=/usr/local/bin/moonfire-nvr run \
    --db-dir=/var/lib/moonfire-nvr/db \
    --http-addr=0.0.0.0:8080 \
    --allow-unauthenticated-permissions='view_video: true'
Environment=TZ=:/etc/localtime
Environment=MOONFIRE_FORMAT=google-systemd
Environment=MOONFIRE_LOG=info
Environment=RUST_BACKTRACE=1
Type=simple
User=moonfire-nvr
Nice=-20
Restart=on-failure
CPUAccounting=true
MemoryAccounting=true
BlockIOAccounting=true

[Install]
WantedBy=multi-user.target
```

Note that the arguments used here are insecure. You can change that via
replacing the `--allow-unauthenticated-permissions` argument here as
described in [Securing Moonfire NVR and exposing it to the
Internet](secure.md).

Some handy commands:

```
$ sudo systemctl daemon-reload                                  # reload configuration files
$ sudo systemctl start moonfire-nvr                             # start the service now
$ sudo systemctl stop moonfire-nvr                              # stop the service now (but don't wait for it finish stopping)
$ sudo systemctl status moonfire-nvr                            # show if the service is running and the last few log lines
$ sudo systemctl enable moonfire-nvr                            # start the service on boot
$ sudo systemctl disable moonfire-nvr                           # don't start the service on boot
$ sudo journalctl --unit=moonfire-nvr --since='-5 min' --follow # look at recent logs and await more
```

See the [systemd](http://www.freedesktop.org/wiki/Software/systemd/)
documentation for more information. The [manual
pages](http://www.freedesktop.org/software/systemd/man/) for `systemd.service`
and `systemctl` may be of particular interest.
