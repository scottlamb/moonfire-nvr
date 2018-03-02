#!/bin/sh -e
#
webpack && test ! -f ui-dist/index.html && ln ui-src/index.html ui-dist/
