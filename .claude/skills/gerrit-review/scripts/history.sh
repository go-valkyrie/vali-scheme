#!/usr/bin/env bash
# shellcheck source-path=SCRIPTDIR
# The change's earlier review, as JSON: every inline comment on any patch
# set and every review message. Each comment carries `thread`, its thread's
# number: threads are numbered by when they started, so a number stays the
# same as later threads are added, and these are the numbers `review.sh
# threads` shows and `review.sh reply` takes.
#
#   history.sh CHANGE
# shellcheck source=gerrit.sh
. "$(dirname "$0")/gerrit.sh"
change_arg "${1:-}"
api="changes/$project_id~$1"
comments=$(gerrit "$api/comments")
messages=$(gerrit "$api/messages")
jq -n --argjson comments "$comments" --argjson messages "$messages" '
  [$comments | to_entries[] | .key as $path | .value[]
    | {id, patch_set, path: $path, line, in_reply_to, unresolved,
       author: (.author.username // .author.name), updated, message}] as $all
  | ($all | map({key: .id, value: .}) | from_entries) as $by
  | def root($c): if $c.in_reply_to and $by[$c.in_reply_to] then root($by[$c.in_reply_to]) else $c end;
  ($all | group_by(root(.).id) | map(sort_by(.updated)) | sort_by(.[0].updated, .[0].id)
   | to_entries | map(.key as $k | .value[] | {key: .id, value: ($k + 1)}) | from_entries) as $number
  | {
      comments: ($all | map(. + {thread: $number[.id]}) | sort_by(.thread, .updated)),
      messages: [$messages[]
        | {patch_set: ._revision_number, author: (.author.username // .author.name),
           date, message}]
    }'
