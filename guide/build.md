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

```console
$ git clone https://github.com/scottlamb/moonfire-nvr.git
$ cd moonfire-nvr
```

## Docker builds

This command should prepare a deployment image for your local machine:

```console
$ sudo docker buildx build --load --tag=moonfire-nvr -f docker/Dockerfile .
```

<details>
  <summary>Common errors</summary>

*   `docker: 'buildx' is not a docker command.`: You shouldn't see this with
    Docker 20.10. With Docker version 19.03 you'll need to prepend
    `DOCKER_CLI_EXPERIMENTAL=enabled` to `docker buildx build` commands. If
    your Docker version is older than 19.03, you'll need to upgrade.
*   `At least one invalid signature was encountered.`: this is likely
    due to an error in `libseccomp`, as described [in this askubuntu.com answer](https://askubuntu.com/a/1264921/1365248).
    Try running in a privileged builder. As described in [`docker buildx build` documentation](https://docs.docker.com/engine/reference/commandline/buildx_build/#allow),
    run this command once:
    ```console
    $ sudo docker buildx create --use --name insecure-builder --buildkitd-flags '--allow-insecure-entitlement security.insecure'
    ```
    then add `--allow security.insecure` to your `docker buildx build` commandlines.
</details>

If you want to iterate on code changes, doing a full Docker build from
scratch every time will be painfully slow. You will likely find it more
helpful to use the `dev` target. This is a self-contained developer environment
which you can use from its shell via `docker run` or via something like
Visual Studio Code's Docker plugin.

```console
$ sudo docker buildx build \
        --load --tag=moonfire-dev --target=dev -f docker/Dockerfile .
...
$ sudo docker run \
        --rm --interactive=true --tty \
        --mount=type=bind,source=$(pwd),destination=/var/lib/moonfire-nvr/src \
        moonfire-dev
```

The development image overrides cargo's output directory to
`/var/lib/moonfire-nvr/target`. (See `~moonfire-nvr/.buildrc`.) This avoids
using a bind filesystem for build products, which can be slow on macOS. It
also means that if you sometimes compile directly on the host and sometimes
within Docker, they don't trip over each other's target directories.

You can also cross-compile to a different architecture. Adding a
`--platform=linux/arm64/v8,linux/arm/v7,linux/amd64` argument will compile
Moonfire NVR for all supported platforms. (Note: this has been used
successfully for building on x86-64 and compiling to arm but not the
reverse.) For the `dev` target, this prepares a build which executes on your
local architecture and is capable of building a binary for your desired target
architecture.

On the author's macOS machine with Docker desktop 3.0.4, building for
multiple platforms at once will initially fail with the following error:

```console
$ sudo docker buildx build ... --platform=linux/arm64/v8,linux/arm/v7,linux/amd64
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
    $ sudo docker buildx build --platform=linux/arm64/v8,linux/arm/v7,linux/amd64 ...
    $ sudo docker buildx build --load --platform=arm64/v8 ...
    ```

On Linux hosts (as opposed to when using Docker Desktop on macOS/Windows),
you'll likely see errors like the ones below. The solution is to [install
emulators](https://github.com/tonistiigi/binfmt#installing-emulators).
You may need to reinstall emulators on each boot of the host.

```
Exec format error

Error while loading /usr/sbin/dpkg-split: No such file or directory
Error while loading /usr/sbin/dpkg-deb: No such file or directory
```

Moonfire NVR's `Dockerfile` has some built-in debugging tools:

*   Each stage saves some debug info to `/docker-build-debug/<stage>`, and
    the `deploy` stage preserves the output from previous stages. The debug
    info includes:
    *    output (stdout + stderr) from the build script, running long operations
         through the `time` command.
    *    `find -ls` output on cache mounts before and after.
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

*   [SQLite3](https://www.sqlite.org/), at least version 3.8.2.
    (You can skip this if you compile with `--features=bundled` and
    don't mind the `moonfire-nvr sql` command not working.)

*   [`ncursesw`](https://www.gnu.org/software/ncurses/), the UTF-8 version of
    the `ncurses` library.

To build the UI, you'll need a [nodejs](https://nodejs.org/en/download/) release
in "Maintenance" or "LTS" status: currently v14, v16, or v18.

On recent Ubuntu or Raspbian Linux, the following command will install
most non-Rust dependencies:

```console
$ sudo apt-get install \
               build-essential \
               libncurses-dev \
               libsqlite3-dev \
               pkgconf \
               sqlite3 \
               tzdata
```

Ubuntu 20.04 LTS (still popular, supported by Ubuntu until April 2025) bundles
node 10, which has reached end-of-life (see
[node.js: releases](https://nodejs.org/en/about/releases/)).
So rather than install the `nodejs` and `npm` packages from the built-in
repository, see [Installing Node.js via package
manager](https://nodejs.org/en/download/package-manager/#debian-and-ubuntu-based-linux-distributions).

On macOS with [Homebrew](https://brew.sh/) and Xcode installed, try the
following command:

```console
$ brew install node
```

Next, you need Rust 1.64+ and Cargo. The easiest way to install them is by
following the instructions at [rustup.rs](https://www.rustup.rs/). Avoid
your Linux distribution's Rust packages, which tend to be too old.
(At least on Debian-based systems; Arch and Gentoo might be okay.)

Once prerequisites are installed, you can build the server and find it in
`target/release/moonfire-nvr`:

```console
$ cd server
$ cargo test
$ cargo build --release
$ sudo install -m 755 target/release/moonfire-nvr /usr/local/bin
$ cd ..
```

You can build the UI via `npm` and find it in the `ui/build` directory:

```console
$ cd ui
$ npm install
$ npm run build
$ sudo mkdir /usr/local/lib/moonfire-nvr
$ cd ..
$ sudo rsync --recursive --delete --chmod=D755,F644 ui/build/ /usr/local/lib/moonfire-nvr/ui
```

### Running interactively straight from the working copy

The author finds it convenient for local development to set up symlinks so that
the binaries in the working copy will run via just `nvr`:

```console
$ sudo mkdir /usr/local/lib/moonfire-nvr
$ sudo ln -s `pwd`/ui/build /usr/local/lib/moonfire-nvr/ui
$ sudo mkdir /var/lib/moonfire-nvr
$ sudo chown $USER: /var/lib/moonfire-nvr
$ ln -s `pwd`/server/target/release/moonfire-nvr $HOME/bin/moonfire-nvr 
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

```ini
[Unit]
Description=Moonfire NVR
After=network-online.target

[Service]
ExecStart=/usr/local/bin/moonfire-nvr run
Environment=TZ=:/etc/localtime
Environment=MOONFIRE_FORMAT=google-systemd
Environment=MOONFIRE_LOG=info
Environment=RUST_BACKTRACE=1
Type=simple
User=moonfire-nvr
Restart=on-failure
CPUAccounting=true
MemoryAccounting=true
BlockIOAccounting=true

[Install]
WantedBy=multi-user.target
```

You'll also need a `/etc/moonfire-nvr.toml`:

```toml
[[binds]]
ipv4 = "0.0.0.0:8080"
allowUnauthenticatedPermissions = { viewVideo = true }

[[binds]]
unix = "/var/lib/moonfire-nvr/sock"
ownUidIsPrivileged = true
```

Note this configuration is insecure. You can change that via replacing the
`allowUnauthenticatedPermissions` here as described in [Securing Moonfire NVR
and exposing it to the Internet](secure.md).

See [ref/config.md](../ref/config.md) for more about `/etc/moonfire-nvr.toml`.

Some handy commands:

```console
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
