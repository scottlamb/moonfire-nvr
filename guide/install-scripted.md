# Building and installing Moonfire NVR using provided scripts

This guide will walk you through building and installing Moonfire NVR with the
provided scripts. You should have already downloaded the source code as
mentioned in [install.md](install.md), and after completing these instructions
you should go back to that page to complete configuration.

There are no binary packages of Moonfire NVR available yet, so it must be built
from source. This is made easy using a few scripts that will do the job for you
unless you have very a different operating system. The scripts are written and
tested under ubuntu and raspbian but should not be hard to modify if necessary.

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

The script may be given the `-f` option. If you do, you are telling the script
that you do not want any existing installation of ffmpeg to be overwritten with
a newer one. This could be important to you. If you do use it, and the version
you have installed is not compatible with Moonfire, you will be told through
a message. If you have no ffmpeg installed, the option is effectively ignored
and the necessary version of ffmpeg will be installed.

The setup script should only need to be run once (after `prep.config` has been
created), although if you do a re-install of Moonfire, in particular a much
newer version, it is a good idea to run it again as requirements and pre-requisites
may have changed. Running the script multiple times should not have any negative effects.

*Note:* It is quite possible that during the running of the setup script,
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

There are two options you may pass to this script. The first is `-B` which
means "build only". In other words, this will stop the automatic invocation
of the install script. The other option available is `-t` and causes the
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
by using the `-s` option to this script. It will also prevent the automatic
start of this configuration.

## Configuration variables

Although not all listed in the default `prep.config` file, these are the
available configuration variable and their defaults.

    NVR_USER=moonfire-nvr
    NVR_PORT=8080
    NVR_HOME_BASE=/var/lib
    DB_NAME=db
    DB_DIR=$NVR_HOME/$DB_NAME
    SERVICE_NAME=moonfire-nvr
    SERVICE_DESC="Moonfire NVR"
    SERVICE_BIN=/usr/local/bin/$SERVICE_NAME

## Completing installation

After the steps on this page, go back to [Downloading, installing, and
configuring Moonfire NVR](install.md) to set up the sample file directory and
configure the system.
