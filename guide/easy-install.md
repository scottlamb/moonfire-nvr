# Installing Moonfire NVR using provided scripts

There are no binary packages of Moonfire NVR available yet, so it must be built
from source. This is made easy using a few scripts that will do the job for you
unless you have very a different operating system. The scripts are written and
tested under ubuntu and raspbian but should not be hard to modify if necessary.
You'll start by downloading Moonfire if you have not already done so.

## Downloading

See the [github page](https://github.com/scottlamb/moonfire-nvr) (in case
you're not reading this text there already). You can download the bleeding
edge version from the command line via git:

    $ git clone https://github.com/scottlamb/moonfire-nvr.git

## Preparation steps for easy install

There are a few things to prepare if you want a truly turnkey install, but
they are both optional.

### Dedicated media directory

An optional, but strongly suggested, step is to setup a dedicated hard disk
for recording video.
Moonfire works best if the video samples are collected on a hard drive of
sufficient capacity and separate from the root and main file systems. This
is particularly important on Raspberry Pi based systems as the flash based
main file systems have a limited lifetime and are way too small to hold
any significant amount of video.
If a dedicated hard drive is available, set up the mount point (in this 
example we'll use /media/nvr/samples):

    $ sudo vim /etc/fstab
    $ sudo mount /mount/media/samples

In the fstab you would add a line similar to this:

    /dev/disk/by-uuid/23d550bc-0e38-4825-acac-1cac8a7e091f    /media/nvr   ext4    defaults,noatime  0       1

You'll have to lookup the correct uuid for your disk. One way to do that is
to issue the following commands:

    $ ls -l /dev/disk/by-uuid

Locate the device where your disk will be mounted (or is mounted), for example
`/dev/sda1`. Now lookup the filename linked to that from the output of the
`ls` command. This is the uuid you need.

The setup script (see below) will create the necessary sample file dir on the mounted
hard disk.


## Setting everything up

Start by executing the setup script:

    $ cd moonfire-nvr
    $ scripts/setup-ubuntu.sh

If this is the very first time you run this script, a file named `prep.config`
will be created and the script will stop. This file is where you will set
or change variables that describe the Moonfire installation you want. The
initial execution will put default values in this value, but only for the
most commonly changed variables. For a full list of variables, see below.

Once you modify this file (if desired), you run the setup script again. This
time it will use the values in the file and proceed with the setup.
The script will download and install any pre-requisites. Watch carefully for
error messages. It may be you have conflicting installations. If that is the
case you must either resolve those first, or go the manual route.

The script may be given the "-f" option. If you do, you are telling the script
that you do not want any existing installation of ffmpeg to be overwritten with
a newer one. This could be important to you. If you do use it, and the version
you have installed is not compatible with Moonfire, you will be told through
a message. If you have no ffmpeg installed, the option is effectively ignored
and the necessary version of ffmpeg will be installed.

The setup script should only need to be run once (after `prep.config` has been
created), although if you do a re-install of Moonfire, in particular a much
newer version, it is a good idea to run it again as requirements and pre-requisites
may have changed. Running the script multiple times should not have any negative effects.

*WARNING* It is quite possible that during the running of the setup script,
in particular during the building of libavutil you will see several compiler
warnings. This, while undesirable, is a direct result of the original
developers not cleaning up the cause(s) of these warnings. They are, however,
just warnings and will not affect correct functioning of Moonfire.

Once the setup is complete, two steps remain: building and then installing.
There is a script for each of these scenarios, but since generally you would
want to install after a succesul build, the build script automatically invokes
the install script, unless specifically told not to.

## Building

The build script is involved like this:

    $ scripts/build.sh

This script will perform all steps necessary to build a complete Moonfire
setup. If there are no build errors, this script will then automatically
invoke the install script (see below).

There are two options you may pass to this script. The first is "-B" which
means "build only". In other words, this will stop the automatic invocation
of the install script. The other option available is "-t" and causes the
script to ignore the results of any tests. In other words, even if tests
fail, the build phase will be considered successful. This can occasionally
be useful if you are doing development, and have temporarily broken one
or more test, but want to proceed anyway.

## Installing

The install step is performed by the script that can be manually invoked
like this:

    $ scripts/install.sh

This script will copy various files resulting from the build to the correct
locations. It will also create a "service configuration" for systemctl that
can be used to control Moonfire. This service configuration can be prevented
by using the "-s" option to this script. It will also prevent the automatic
start of this configuration.


## Configuration variables

Although not all listed in the default prep.config file, these are the
available configuration variable and their defaults. In the most frequent
scenarios you will probably only change SAMPLE_MEDIA_DIR to point
to your mounted external disk (/media/nvr in the example above).

    NVR_USER=moonfire-nvr
    NVR_GROUP=$NVR_USER
    NVR_PORT=8080
    NVR_HOME_BASE=/var/lib
    DB_NAME=db
    DB_DIR=$NVR_HOME/$DB_NAME
    SAMPLE_FILE_DIR=sample
    SAMPLE_MEDIA_DIR=$NVR_HOME
    SERVICE_NAME=moonfire-nvr
    SERVICE_DESC="Moonfire NVR"
    SERVICE_BIN=/usr/local/bin/$SERVICE_NAME
