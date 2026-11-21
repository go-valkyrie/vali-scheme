#!/usr/bin/env bash
# shellcheck source-path=SCRIPTDIR
# Every open change of the project other than CHANGE, as JSON: number,
# patch set, owner, subject, commit message and the files it changes (no
# diffs). Without CHANGE, every open change.
#
#   open-changes.sh [CHANGE]
# shellcheck source=gerrit.sh
. "$(dirname "$0")/gerrit.sh"
[ -z "${1:-}" ] || change_arg "$1"
gerrit "changes/?q=project:$project_id+status:open&o=CURRENT_REVISION&o=CURRENT_COMMIT&o=CURRENT_FILES&o=DETAILED_ACCOUNTS&n=200" |
  jq --argjson self "${1:-0}" '[.[] | select(._number != $self)
    | .revisions[.current_revision] as $r
    | {change: ._number, patch_set: $r._number,
       owner: (.owner.username // .owner.name // .owner._account_id),
       subject, message: $r.commit.message, files: ($r.files // {} | keys)}]'
