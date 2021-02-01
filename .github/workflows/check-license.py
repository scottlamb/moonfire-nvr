#!/usr/bin/env python3
# This file is part of Moonfire NVR, a security camera network video recorder.
# Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
# SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception
"""Checks that expected header lines are present.

Call in either of two modes:

has-license.py FILE [...]
    check if all files with certain extensions have expected lines.
    This is useful in a CI action.

has-license.py
    check if stdin has expected lines.
    This is useful in a pre-commit hook, as in
    git-format-staged --no-write --formatter '.../has-license.py' '*.rs'
"""
import re
import sys

# Filenames matching this regexp are expected to have the header lines.
FILENAME_MATCHER = re.compile(r'.*\.([jt]sx?|html|css|py|rs|sh|sql)$')

MAX_LINE_COUNT = 10

EXPECTED_LINES = [
  re.compile(r'This file is part of Moonfire NVR, a security camera network video recorder\.'),
  re.compile(r'Copyright \(C\) 20\d{2} The Moonfire NVR Authors; see AUTHORS and LICENSE\.txt\.'),
  re.compile(r'SPDX-License-Identifier: GPL-v3\.0-or-later WITH GPL-3\.0-linking-exception\.?'),
]

def has_license(f):
  """Returns if all of EXPECTED_LINES are present within the first
  MAX_LINE_COUNT lines of f."""
  needed = set(EXPECTED_LINES)
  i = 0
  for line in f:
    if i == 10:
      break
    i += 1
    for e in needed:
      if e.search(line):
        needed.remove(e)
        break
    if not needed:
      return True
  return False


def file_has_license(filename):
  with open(filename, 'r') as f:
    return has_license(f)
    

def main(args):
  if not args:
    sys.exit(0 if has_license(sys.stdin) else 1)

  missing = [f for f in args
             if FILENAME_MATCHER.match(f) and not file_has_license(f)]
  if missing:
    print('The following files are missing expected copyright/license headers:')
    print('\n'.join(missing))
    sys.exit(1)


if __name__ == '__main__':
  main(sys.argv[1:])
