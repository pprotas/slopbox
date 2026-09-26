#!/usr/bin/env bash
set -euo pipefail

"${OTOOL:-otool}" -L "${1:?Slopbox executable required}" |
  awk '
    NR > 1 && $1 !~ /^\/(usr\/lib|System\/Library)\// {
      print "non-system worker dependency: " $1 > "/dev/stderr"
      failed = 1
    }
    END { exit failed || NR < 2 }
  '
