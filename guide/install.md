# Downloading, installing, and configuring Moonfire NVR

This document describes how to download, install, and configure Moonfire NVR
on a Debian-based Linux system (such as Ubuntu or Raspbian).

(In principle, Moonfire NVR supports any POSIX-compliant system, and the main
author uses macOS for development, but the documentation and scripts are
intended for Linux.)

## Downloading

See the [github page](https://github.com/scottlamb/moonfire-nvr) (in case
you're not reading this text there already). You can download the
bleeding-edge version from the commandline via git:

```
$ git clone https://github.com/scottlamb/moonfire-nvr.git
```

## Building and installing from source

There are no binary packages of Moonfire NVR available yet, so it must be built
from source. To do so, you can follow either of two paths:

   * Scripted: You will run some shell scripts (after preparing one or two files,
     and will be completely done. This is by far the easiest option, in
     particular for first time builders/installers. Read more in [Scripted
     Installation](install-scripted.md).
   * Manual: see [instructions](install-manual.md).

Moonfire NVR keeps two kinds of state:

   * a SQLite database, typically <1 GiB. It should be stored on flash if
     available.
   * the "sample file directories", which hold the actual samples/frames of
     H.264 video. These should be quite large and are typically stored on hard
     drives.

(See [schema.md](schema.md) for more information.)

By now Moonfire NVR's dedicated user and database should have been created for
you. Next you need to create a sample file directory.

## Dedicated hard drive seutp

If a dedicated hard drive is available, set up the mount point:

```
$ sudo vim /etc/fstab
$ sudo mkdir /media/nvr
$ sudo mount /media/nvr
$ sudo install -d -o moonfire-nvr -g moonfire-nvr -m 700 /media/nvr/sample
```

In `/etc/fstab`, add a line similar to this:

```
/dev/disk/by-uuid/23d550bc-0e38-4825-acac-1cac8a7e091f    /media/nvr   ext4    defaults,noatime,nofail  0       2
```

You'll have to lookup the correct uuid for your disk. One way to do that is
via the following command:

```
$ ls -l /dev/disk/by-uuid
```

If you use the `nofail` attribute in `/etc/fstab` as described above, your
system will boot successfully even when the hard drive is unavailable (such as
when your external USB storage is unmounted). This is convenient, but you
likely want to ensure the `moonfire-nvr` service only starts when the mounting
is successful. Edit the systemd configuration to do so:

```
$ sudo vim /etc/systemd/system/moonfire-nvr.service
$ sudo systemctl daemon-reload
```

You'll want to add a line similar to the following to the `[Unit]` section of
the file:

```
RequiresMountsFor=/media/nvr
```

## Completing configuration through the UI

Once setup is complete, it is time to add sample file directory and camera
configurations to the database.

You can configure the system's database through a text-based user interface:

```
$ sudo -u moonfire-nvr moonfire-nvr config 2>debug-log
```

In the user interface,

 1. add your sample file dir(s) under "Directories and retention".
    If you used a dedicated hard drive, use the directory you precreated
    (`/media/surveillance/sample`). Otherwise, try
    `/var/lib/moonfire-nvr/sample`. Moonfire NVR will create the directory as
    long as it has the required permissions on the parent directory.

 2. add cameras under "Cameras and streams".

    * See the [wiki](https://github.com/scottlamb/moonfire-nvr/wiki) for notes
      about specific camera models.

    * There's a "Test" button to verify your settings directly from the add/edit
      camera dialog.

    * Be sure to assign each stream you want to capture to a sample file
      directory and check the "record" box.

    * `flush_if_sec` should typically be 120 seconds. This causes the database to
      be flushed when the first instant of one of this stream's completed
      recordings is 2 minutes old. A "recording" is a segment of a video
      stream that is 60–120 seconds when first establishing the stream, about
      60 seconds midstream, and shorter when an error or server shutdown
      terminates the stream. Thus, a value just below 60 will cause the
      database to be flushed once per minute per stream in the steady state. A
      value around 180 will cause the database to be once every 3 minutes per
      stream, or less frequently if other streams cause flushes first. Lower
      values cause less video to be lost on power loss. Higher values reduce
      wear on the SSD holding the SQLite database, particularly when you have
      many cameras and when you record both the "main" and "sub" streams of
      each camera.

 3. Assign disk space to your cameras back in "Directories and retention".
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

 4. Add a user for yourself (and optionally others) under "Users". You'll need
    this to access the web UI once you enable authentication.

## Starting it up

Note that at this stage, Moonfire NVR's web interface is **insecure**: it
doesn't use `https` and doesn't require you to authenticate
to it. You might be comfortable starting it in this configuration to try it
out, particularly if the machine it's running on is behind a home router's
firewall. You might not; in that case read through [secure the
system](secure.md) first.

The following commands will start Moonfire NVR and enable it for following
boots, respectively:

```
$ sudo systemctl start moonfire-nvr
$ sudo systemctl enable moonfire-nvr
```

The HTTP interface is accessible on port 8080; if your web browser is running
on the same machine, you can access it at
[http://localhost:8080/](http://localhost:8080/).

If the system isn't working, see the [Troubleshooting
guide](troubleshooting.md).

Once the web interface seems to be working, read through [securing Moonfire
NVR](secure.md).
