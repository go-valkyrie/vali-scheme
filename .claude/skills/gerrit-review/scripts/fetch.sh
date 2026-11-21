#!/usr/bin/env bash
# shellcheck source-path=SCRIPTDIR
# Fetches a patch set's commit, and its parent, into the current repository
# and prints the commit's hash. It leaves no ref behind: use the hash.
#
#   fetch.sh CHANGE PATCHSET
# shellcheck source=gerrit.sh
. "$(dirname "$0")/gerrit.sh"
change_arg "${1:-}" "PATCHSET"
case "${2:-}" in "" | *[!0-9]*) echo "usage: $(basename "$0") CHANGE PATCHSET" >&2; exit 2 ;; esac
nn=$(printf '%02d' $(($1 % 100)))
git fetch -q --no-tags --depth=2 "$repo_url" "refs/changes/$nn/$1/$2"
git rev-parse FETCH_HEAD
