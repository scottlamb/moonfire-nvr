# Building Moonfire NVR <!-- omit in toc -->

This document has notes for software developers on building Moonfire NVR from
source code for development. If you just want to install precompiled
binaries, see the [installation instructions](install.md) instead.

This document doesn't spell out as many details as the installation
instructions. Please ask on Moonfire NVR's [issue
tracker](https://github.com/scottlamb/moonfire-nvr/issues) or
[mailing list](https://groups.google.com/d/forum/moonfire-nvr-users) when
stuck. Please also send pull requests to improve this doc.

* [Downloading](#downloading)
* [Building](#building)
    * [Running interactively straight from the working copy](#running-interactively-straight-from-the-working-copy)
* [Release procedure](#release-procedure)

## Downloading

See the [github page](https://github.com/scottlamb/moonfire-nvr) (in case
you're not reading this text there already). You can download the
bleeding-edge version from the commandline via git:

```console
$ git clone https://github.com/scottlamb/moonfire-nvr.git
$ cd moonfire-nvr
```

## Building

Moonfire NVR should run natively on any Unix-like system. It's been tested on
Linux, macOS, and FreeBSD. (In theory [Windows Subsystem for
Linux](https://docs.microsoft.com/en-us/windows/wsl/about) should also work.
Please speak up if you try it.)

To build the server, you will need [SQLite3](https://www.sqlite.org/). You
can skip this if compiling with `--features=rusqlite/bundled` and don't
mind the `moonfire-nvr sql` command not working.

To build the UI, you'll need a [nodejs](https://nodejs.org/en/download/) release
in "Maintenance", "LTS", or "Current" status on the
[Release Schedule](https://github.com/nodejs/release#release-schedule):
currently v18, v20, or v21.

On recent Ubuntu or Raspbian Linux, the following command will install
most non-Rust dependencies:

```console
$ sudo apt-get install \
               build-essential \
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

Next, you need Rust 1.79+ and Cargo. The easiest way to install them is by
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

You can build the UI via `pnpm` and find it in the `ui/build` directory:

```console
$ cd ui
$ pnpm install
$ pnpm run build
$ sudo mkdir /usr/local/lib/moonfire-nvr
$ cd ..
$ sudo rsync --recursive --delete --chmod=D755,F644 ui/dist/ /usr/local/lib/moonfire-nvr/ui
```

For more information about using `pnpm`, check out the [Developing UI Guide](./developing-ui.md#requirements).

If you wish to bundle the UI into the binary, you can build the UI first and then pass
`--features=bundled-ui` when building the server. See also the
[release workflow](../.github/workflows/release.yml) which statically links SQLite and
(musl-based) libc for a zero-dependencies binary.

### Running interactively straight from the working copy

The author finds it convenient for local development to set up symlinks so that
the binaries in the working copy will run via just `nvr`:

```console
$ sudo mkdir /usr/local/lib/moonfire-nvr
$ sudo ln -s `pwd`/ui/dist /usr/local/lib/moonfire-nvr/ui
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

## Release procedure

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
    git push origin "v${VERSION}"
    ```

The rest should happen automatically—the tag push will fire off a GitHub
Actions workflow which creates a release, cross-compiles statically compiled
binaries for three different platforms, and uploads them to the release.
