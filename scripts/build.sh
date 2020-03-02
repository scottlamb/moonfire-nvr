#!/bin/bash
#
# This file is part of Moonfire NVR, a security camera network video recorder.
# Copyright (C) 2016-17 The Moonfire NVR Authors
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
		echo_fatal -x "Option -$OPTARG requires an argument."
		;;
	  \?)
		echo_fatal "Invalid option: -$OPTARG"
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
	echo_fatal -x "rustc not present or version less than $RUSTC_MIN_VERSION"
fi

cv=$(getVersion cargo 0.0)
if ! versionAtLeast "$cv" "$CARGO_MIN_VERSION"; then
	echo_fatal -x "cargo not present or version less than $CARGO_MIN_VERSION"
fi

yv=$(getVersion yarn 0.0)
if ! versionAtLeast "$yv" "$YARN_MIN_VERSION"; then
	echo_fatal -x "yarn not present or version less than $YARN_MIN_VERSION"
fi

# Building main server component
#
if [ "${FORCE_CLEAN:-0}" -eq 1 ]; then
	echo_info -x "Forcing clean server build..."
	cargo clean
fi

echo_info -x "Building test version..."
if ! cargo test; then
	echo_error -x "test failed." "Try to run the following manually for more info" \
			 "RUST_TEST_THREADS=1 cargo test --verbose" ''
	if [ "${IGNORE_TESTS:-0}" -ne 1 ]; then
		exit 1
	fi
fi
if ! cargo build --release; then
	echo_error -x "Server/release build failed." "Try to run the following manually for more info" \
			 "cargo build --release --verbose" ''
	exit 1
fi

# Building UI components
#
echo_info -x "Building UI components..."
if ! yarn build; then
	echo_fatal -x "UI build failed." "yarn build" 
fi

# Stop if build only is desired
#
if [ "${BUILD_ONLY:-0}" != 0 ]; then
        echo_info -x "Build (only) complete, exiting"
        exit 0
fi
. `dirname ${BASH_SOURCE[0]}`/install.sh
