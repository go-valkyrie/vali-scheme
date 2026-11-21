#!/usr/bin/env bash
# shellcheck source-path=SCRIPTDIR
# The change's relation chain, as JSON, parents first, with `this: true` on
# CHANGE. Open members carry their commit message and files. An empty list
# when the change stands alone. REV is the patch set's commit; without it,
# the change's current patch set.
#
#   stack.sh CHANGE [REV]
# shellcheck source=gerrit.sh
. "$(dirname "$0")/gerrit.sh"
change_arg "${1:-}" "[REV]"
related=$(gerrit "changes/$project_id~$1/revisions/${2:-current}/related")
open=$("$(dirname "$0")/open-changes.sh")
jq --argjson self "$1" --argjson open "$open" '
  ($open | map({key: (.change | tostring), value: .}) | from_entries) as $by
  | [.changes | reverse[]
     | {change: ._change_number, patch_set: ._revision_number, status,
        subject: .commit.subject, this: (._change_number == $self)}
     + (($by[(.change | tostring)] // {}) | {message, files})]' <<< "$related"
