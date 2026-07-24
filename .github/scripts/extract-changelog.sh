#!/usr/bin/env bash

set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "Usage: $0 <changelog-path> <version>" >&2
  exit 1
fi

changelog=$1
version=$2

if [[ ! -f "$changelog" ]]; then
  echo "Changelog not found: $changelog" >&2
  exit 1
fi

awk -v version="$version" '
  /^## / {
    if (in_section) exit
    if ($2 == version) {
      in_section = 1
      next
    }
  }
  in_section { print }
' "$changelog"
