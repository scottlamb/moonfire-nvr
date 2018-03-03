#!/bin/bash
#
# This file is part of Moonfire NVR, a security camera network video recorder.
# Copyright (C) 2016-17 Scott Lamb <slamb@slamb.org>
#
# This program is free software: you can redistribute it and/or modify
# it under the terms of the GNU General Public License as published by
# the Free Software Foundation, either version 3 of the License, or
# (at your option) any later version.
#
# In addition, as a special exception, the copyright holders give
# permission to link the code of portions of this program with the
# OpenSSL library under certain conditions as described in each
# individual source file, and distribute linked combinations including
# the two.
#
# You must obey the GNU General Public License in all respects for all
# of the code used other than OpenSSL. If you modify file(s) with this
# exception, you may extend this exception to your version of the
# file(s), but you are not obligated to do so. If you do not wish to do
# so, delete this exception statement from your version. If you delete
# this exception statement from all source files in the program, then
# also delete it here.
#
# This program is distributed in the hope that it will be useful,
# but WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
# GNU General Public License for more details.
#
# You should have received a copy of the GNU General Public License
# along with this program.  If not, see <http://www.gnu.org/licenses/>.
#

. `dirname ${BASH_SOURCE[0]}`/script-functions.sh

initEnvironmentVars

# Process command line options
#
while getopts ":Bt" opt; do
	case $opt in
	  B)    BUILD_ONLY=1
		;;
	  t)    IGNORE_TESTS=1
		;;
	  :)
		echo "Option -$OPTARG requires an argument." >&2
		exit 1
		;;
	  \?)
		echo "Invalid option: -$OPTARG" >&2
		exit 1
		;;
	esac
done

# Setup cargo if files are present
#
initCargo

# Check environment
#

rv=$(getVersion rustc 0.0)
if ! versionAtLeast "$rv" "$RUSTC_MIN_VERSION"; then
	echo "rustc not present or version less than $RUSTC_MIN_VERSION"
	exit 1
fi

cv=$(getVersion cargo 0.0)
if ! versionAtLeast "$cv" "$CARGO_MIN_VERSION"; then
	echo "cargo not present or version less than $CARGO_MIN_VERSION"
	exit 1
fi

yv=$(getVersion yarn 0.0)
if ! versionAtLeast "$yv" "$YARN_MIN_VERSION"; then
	echo "yarn not present or version less than $YARN_MIN_VERSION"
	exit 1
fi

# Building main server component
#
if [ "${FORCE_CLEAN:-0}" -eq 1 ]; then
	echo "Forcing clean server build..."; echo
	cargo clean
fi

echo "Building test version..."; echo
if ! cargo test; then
	echo "test failed."
	echo "Try to run the following manually for more info"
	echo "RUST_TEST_THREADS=1 cargo test --verbose"
	echo
	if [ "${IGNORE_TESTS:-0}" -ne 1 ]; then
		exit 1
	fi
fi
if ! cargo build --release; then
	echo "Server/release build failed."
	echo "Try to run the following manually for more info"
	echo "RUST_TEST_THREADS=1 cargo build --release --verbose"
	echo
	exit 1
fi

# Building UI components
#
echo "Building UI components..."; echo
if ! yarn build; then
	echo "UI build failed."
	echo "yarn build"
	echo
	exit 1
fi

# Stop if build only is desired
#
if [ "${BUILD_ONLY:-0}" != 0 ]; then
        echo "Build (only) complete, exiting"; echo
        exit 0
fi
. `dirname ${BASH_SOURCE[0]}`/install.sh
