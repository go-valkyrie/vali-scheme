#!/usr/bin/env bash
# One GitHub issue, as JSON: number, title, type, milestone, state, body
# and its comments. GitHub answers without credentials; GH_TOKEN or
# GITHUB_TOKEN, when set, raises its rate limit. REPOSITORY is
# "owner/name", by default Vali's.
#
#   issue.sh NUMBER
set -euo pipefail
case "${1:-}" in "" | *[!0-9]*) echo "usage: $(basename "$0") NUMBER" >&2; exit 2 ;; esac
repository=${REPOSITORY:-go-valkyrie/vali-scheme}
token=${GH_TOKEN:-${GITHUB_TOKEN:-}}
auth=()
[ -z "$token" ] || auth=(-H "Authorization: Bearer $token")
api="https://api.github.com/repos/$repository/issues/$1"
get() {
  curl -fsS --retry 2 ${auth[@]+"${auth[@]}"} -H 'Accept: application/vnd.github+json' "$1"
}
issue=$(get "$api")
comments=$(get "$api/comments?per_page=100")
jq -n --argjson i "$issue" --argjson c "$comments" '{
  number: $i.number, title: $i.title, type: $i.type.name,
  milestone: $i.milestone.title, state: $i.state, body: $i.body,
  comments: [$c[] | {author: .user.login, created_at, body}]
}'
