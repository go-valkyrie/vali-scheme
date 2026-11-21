#!/usr/bin/env bash
# Follows a Muse Code session as it runs, printing what Muse does: its
# messages and reasoning, every tool call, each model call's tokens, and
# the same for each subagent a workflow of the session's starts.
#
#   watch-session.sh [SESSION_ID]
#
# With a session ID it waits for that session's log to appear; without one
# it follows the newest session. The review job in gerrit-verify.yml runs it
# alongside Muse, so the job's log shows the review as it happens; it runs
# it as a process group of its own and stops the whole group. Needs jq.
set -u
here=$(cd "$(dirname "$0")" && pwd)
root="${XDG_DATA_HOME:-$HOME/.local/share}/muse/sessions"

log=
if [ $# -gt 0 ]; then
  # Sessions are filed by date: sessions/YYYY/MM/DD/<id>/session.jsonl.
  until log=$(find "$root" -mindepth 5 -maxdepth 5 -path "*/$1/session.jsonl" -print -quit 2>/dev/null) && [ -n "$log" ]; do
    sleep 1
  done
else
  # Session directories are UUIDs, and only ls sorts by time on both Linux
  # and macOS.
  # shellcheck disable=SC2012
  newest=$(ls -td "$root"/*/*/*/*/ 2>/dev/null | head -n 1)
  [ -n "$newest" ] || { echo "no Muse sessions under $root" >&2; exit 1; }
  log="${newest}session.jsonl"
fi

# Prints the session log $1 as it grows, each line starting with $2.
follow() {
  tail -n +1 -F "$1" 2>/dev/null | jq -r --unbuffered --arg prefix "$2" -f "$here/session-log.jq"
}

follow "$log" "" &

# A workflow's subagents run as sessions of their own, under the session's
# subagent/ directory, while the session waits; with only its own log
# followed, the job's log would go quiet for as long as they work. Each is
# followed as it appears, its lines marked with the start of its id. jq
# writes whole lines, so the logs interleave line by line.
seen=" "
while :; do
  for sub in "${log%/session.jsonl}"/subagent/*/session.jsonl; do
    [ -f "$sub" ] || continue
    id=$(basename "$(dirname "$sub")")
    case $seen in *" $id "*) continue ;; esac
    seen="$seen$id "
    follow "$sub" "[${id%%-*}] " &
  done
  sleep 1
done
