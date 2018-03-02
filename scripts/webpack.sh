#!/bin/sh
#
webpack
RESULT=$?
if [ ! -f ui-dist/index.html ]; then
	ln ui-src/index.html ui-dist/
fi
exit $RESULT
