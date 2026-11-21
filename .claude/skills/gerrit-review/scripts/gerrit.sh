# shellcheck shell=bash
# Sourced by the other scripts: where Gerrit is, and a reader for its REST
# API. GerritHub answers these reads without credentials. REPO_URL is the
# project's URL on Gerrit (the review job sets it); it defaults to Vali's.
set -euo pipefail

repo_url=${REPO_URL:-https://review.gerrithub.io/go-valkyrie/vali-scheme}
project=${repo_url#*://*/}
host=${repo_url%"/$project"}
# The scripts that source this use it.
# shellcheck disable=SC2034
project_id=${project//\//%2F}

# GET a path under the host, without Gerrit's ")]}'" guard line.
gerrit() {
  curl -fsS --retry 2 "$host/$1" | tail -c +5
}

# A change number, or nothing.
change_arg() {
  case "${1:-}" in
    "" | *[!0-9]*) echo "usage: $(basename "$0") CHANGE${2:+ $2}" >&2; exit 2 ;;
  esac
}
