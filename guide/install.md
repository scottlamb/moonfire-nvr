# Installing Moonfire NVR <!-- omit in toc -->

* [Downloading, installing, and configuring Moonfire NVR with Docker](#downloading-installing-and-configuring-moonfire-nvr-with-docker)
    * [Dedicated hard drive setup](#dedicated-hard-drive-setup)
    * [Completing configuration through the UI](#completing-configuration-through-the-ui)
    * [Starting it up](#starting-it-up)

## Downloading, installing, and configuring Moonfire NVR with Docker

This document describes how to download, install, and configure Moonfire NVR
via the prebuilt Docker images available for x86-64, arm64, and arm. If you
instead want to build Moonfire NVR yourself, see the [Build
instructions](build.md).

First, install [Docker](https://www.docker.com/) if you haven't already,
and verify `docker run --rm hello-world` works.

Next, you'll need to set up your filesystem and the Moonfire NVR user.

Moonfire NVR keeps two kinds of state:

*   a SQLite database, typically <1 GiB. It should be stored on flash if
    available. In most cases your root filesystem is on flash, so the
    default location of `/var/lib/moonfire-nvr/db` will be fine.
*   the "sample file directories", which hold the actual samples/frames of
    H.264 video. These should be quite large and are typically stored on hard
    drives. More below.

(See [schema.md](schema.md) for more information.)

On most Linux systems, you can create the user as follows:

```
$ sudo useradd --user-group --create-home --home /var/lib/moonfire-nvr moonfire-nvr
```

and create a script called `nvr` to run Moonfire NVR as the intended host user.
This script supports running Moonfire NVR's various administrative commands interactively
and managing a long-lived Docker container for its web interface.

As you set up this script, adjust the `tz` variable as appropriate for your
time zone.

```
sudo sh -c 'cat > /usr/local/bin/nvr' <<'EOF'
#!/bin/bash -e

tz="America/Los_Angeles"
container_name="moonfire-nvr"
image_name="scottlamb/moonfire-nvr:latest"
common_docker_run_args=(
        --mount=type=bind,source=/var/lib/moonfire-nvr,destination=/var/lib/moonfire-nvr
        --user="$(id -u moonfire-nvr):$(id -g moonfire-nvr)"
        --security-opt=seccomp:unconfined
        --env=RUST_BACKTRACE=1
        --env=TZ=":${tz}"
)

case "$1" in
run)
        shift
        exec docker run \
                --detach=true \
                --restart=on-failure \
                "${common_docker_run_args[@]}" \
                --network=host \
                --name="${container_name}" \
                "${image_name}" \
                run \
                --allow-unauthenticated-permissions='view_video: true' \
                "$@"
        ;;
start|stop|logs|rm)
        exec docker "$@" "${container_name}"
        ;;
pull)
        exec docker pull "${image_name}"
        ;;
*)
        exec docker run \
                --interactive=true \
                --tty \
                --rm \
                "${common_docker_run_args[@]}" \
                "${image_name}" \
                "$@"
        ;;
esac
EOF
sudo chmod a+rx /usr/local/bin/nvr
```

then try it out by initializing the database:

```
$ nvr init
```

This will create a directory `/var/lib/moonfire-nvr/db` with a SQLite3 database
within it.

### Dedicated hard drive setup

If a dedicated hard drive is available, set up the mount point:

```
$ sudo vim /etc/fstab
$ sudo mkdir /media/nvr
$ sudo mount /media/nvr
$ sudo install -d -o moonfire-nvr -g moonfire-nvr -m 700 /media/nvr/sample
```

In `/etc/fstab`, add a line similar to this:

```
UUID=23d550bc-0e38-4825-acac-1cac8a7e091f    /media/nvr   ext4    nofail,noatime,lazytime,data=writeback,journal_async_commit  0       2
```

You'll have to lookup the correct uuid for your disk. One way to do that is
via the following command:

```
$ ls -l /dev/disk/by-uuid
```

If you use the `nofail` attribute in `/etc/fstab` as described above, your
system will boot successfully even when the hard drive is unavailable (such as
when your external USB storage is unmounted). This can be helpful when
recovering from problems.

Create the sample directory.

```
sudo mkdir /media/nvr/sample
sudo chmod a+rw -R /media/nvr
```

Add a new `--mount` line to your Docker wrapper script `/usr/local/bin/nvr`
to expose this new volume to the Docker container, directly below the other
mount lines. It will look similar to this:

```
        --mount=type=bind,source=/media/nvr/sample,destination=/media/nvr/sample
```

### Completing configuration through the UI

Once your system is set up, it's time to initialize an empty database
and add the cameras and sample directories. You can do this
by using the `moonfire-nvr` binary's text-based configuration tool.

```
$ nvr config 2>debug-log
```

In the user interface,

1.  add your sample file dir(s) under "Directories and retention".
    If you used a dedicated hard drive, use the directory you precreated
    (eg `/media/nvr/sample`). Otherwise, try
    `/var/lib/moonfire-nvr/sample`. Moonfire NVR will create the directory as
    long as it has the required permissions on the parent directory.

2.  add cameras under "Cameras and streams".

    *   See the [wiki](https://github.com/scottlamb/moonfire-nvr/wiki) for notes
        about specific camera models.

    *   There's a "Test" button to verify your settings directly from the add/edit
        camera dialog.

    *   Be sure to assign each stream you want to capture to a sample file
        directory and check the "record" box.

    *   `flush_if_sec` should typically be 120 seconds. This causes the database to
        be flushed when the first instant of one of this stream's completed
        recordings is 2 minutes old. A "recording" is a segment of a video
        stream that is 60–120 seconds when first establishing the stream,
        about 60 seconds midstream, and shorter when an error or server
        shutdown terminates the stream. Thus, a value just below 60 will
        cause the database to be flushed once per minute per stream in the
        steady state. A value around 180 will cause the database to be once
        every 3 minutes per stream, or less frequently if other streams cause
        flushes first. Lower values cause less video to be lost on power
        loss. Higher values reduce wear on the SSD holding the SQLite
        database, particularly when you have many cameras and when you record
        both the "main" and "sub" streams of each camera.

3.  Assign disk space to your cameras back in "Directories and retention".
    Leave a little slack between the total limit and the filesystem capacity,
    even if you store nothing else on the disk. 1 GiB per camera should be
    plenty. This is needed for a few reasons:

    *   Up to `max(120, flush_if_sec)` seconds of video can be written before
        being counted toward the usage because the recording doesn't count until
        it's fully written, and old recordings can't be deleted until the
        next database flush. So a 8 Mbps video stream with `flush_if_sec=300`
        will take up to (8 Mbps * 300 sec / 8 bits/byte) = 300 MB ~= 286 MiB
        of extra disk space.
    *   If a file is open when it is deleted (such as if a HTTP client is
        downloading it), it stays around until the file is closed. Moonfire NVR
        currently doesn't account for this.
    *   Smaller factors: deletion isn't instantaneous, and directories
        themselves take up some disk space.

4.  Add a user for yourself (and optionally others) under "Users". You'll need
    this to access the web UI once you enable authentication.

### Starting it up

Note that at this stage, Moonfire NVR's web interface is **insecure**: it
doesn't use `https` and doesn't require you to authenticate
to it. You might be comfortable starting it in this configuration to try it
out, particularly if the machine it's running on is behind a home router's
firewall. You might not; in that case read through [secure the
system](secure.md) first.

This command will start a detached Docker container for the web interface.
It will automatically restart when your system does.

```
$ nvr run
```

You can temporarily disable the service via `nvr stop` and restart it later via
`nvr start`.

The HTTP interface is accessible on port 8080; if your web browser is running
on the same machine, you can access it at
[http://localhost:8080/](http://localhost:8080/).

If the system isn't working, see the [Troubleshooting
guide](troubleshooting.md).

Once the web interface seems to be working, read through [securing Moonfire
NVR](secure.md).
