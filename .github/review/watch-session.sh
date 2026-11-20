#!/usr/bin/env bash
# Follows a Muse Code session as it runs, printing what Muse does: its
# messages and reasoning, every tool call, and each model call's tokens.
#
#   watch-session.sh [SESSION_ID]
#
# With a session ID it waits for that session's log to appear; without one
# it follows the newest session. The review job in gerrit-verify.yml runs it
# alongside Muse, so the job's log shows the review as it happens. Needs jq.
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

tail -n +1 -F "$log" 2>/dev/null | jq -r --unbuffered -f "$here/session-log.jq"
