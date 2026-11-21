#!/usr/bin/env bash
# Every open issue of the GitHub repository Gerrit replicates to, as JSON:
# number, title, type and milestone. GitHub answers without credentials;
# GH_TOKEN or GITHUB_TOKEN, when set, raises its rate limit. REPOSITORY is
# "owner/name", by default Vali's.
#
#   open-issues.sh
set -euo pipefail
repository=${REPOSITORY:-go-valkyrie/vali-scheme}
token=${GH_TOKEN:-${GITHUB_TOKEN:-}}
auth=()
[ -z "$token" ] || auth=(-H "Authorization: Bearer $token")
page=1
all='[]'
while :; do
  batch=$(curl -fsS --retry 2 ${auth[@]+"${auth[@]}"} -H 'Accept: application/vnd.github+json' \
    "https://api.github.com/repos/$repository/issues?state=open&per_page=100&page=$page")
  all=$(jq -n --argjson a "$all" --argjson b "$batch" '$a + $b')
  [ "$(jq length <<< "$batch")" -eq 100 ] || break
  page=$((page + 1))
done
# The issues API lists pull requests too.
jq '[.[] | select(.pull_request | not)
  | {number, title, type: .type.name, milestone: .milestone.title}]
  | sort_by(.number)' <<< "$all"
