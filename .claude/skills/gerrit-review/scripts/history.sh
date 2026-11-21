#!/usr/bin/env bash
# shellcheck source-path=SCRIPTDIR
# The change's earlier review, as JSON: every inline comment on any patch
# set and every review message.
#
#   history.sh CHANGE
# shellcheck source=gerrit.sh
. "$(dirname "$0")/gerrit.sh"
change_arg "${1:-}"
api="changes/$project_id~$1"
comments=$(gerrit "$api/comments")
messages=$(gerrit "$api/messages")
jq -n --argjson comments "$comments" --argjson messages "$messages" '{
  comments: [$comments | to_entries[] | .key as $path | .value[]
    | {id, patch_set, path: $path, line, in_reply_to, unresolved,
       author: (.author.username // .author.name), updated, message}]
    | sort_by(.updated),
  messages: [$messages[]
    | {patch_set: ._revision_number, author: (.author.username // .author.name),
       date, message}]
}'
