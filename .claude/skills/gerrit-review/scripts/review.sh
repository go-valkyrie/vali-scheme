#!/usr/bin/env bash
# shellcheck source-path=SCRIPTDIR
# The jq filters name jq's variables ($t, $m, ...), not the shell's.
# shellcheck disable=SC2016
# Builds a review answer one command at a time, checking each as it goes, so
# no one writes the answer's JSON by hand. The answer is what the review job
# posts: {review, codeReview, comments}, in .review/answer.json at the
# repository's root (REVIEW_DIR moves it).
#
#   review.sh init [--force] CHANGE PATCHSET [REV]
#                                          start an answer; fetches REV if needed
#   review.sh threads                      the earlier threads, numbered
#   review.sh reply N [--resolved | --unresolved]   message on stdin
#   review.sh comment PATH LINE|- [--resolved | --unresolved]   message on stdin
#   review.sh drop K                       remove comment K (as `show` numbers it)
#   review.sh vote -1|0|1
#   review.sh summary                      the summary, on stdin
#   review.sh show                         the answer as it stands
#   review.sh check [FILE]                 is it complete and well formed
#
# A reply goes on the thread's latest comment, on that comment's patch set,
# file and line; a new comment goes on this patch set. Without a flag, a
# reply leaves the thread as it is and a new comment opens one (unresolved).
# shellcheck source=gerrit.sh
. "$(dirname "$0")/gerrit.sh"

here=$(cd "$(dirname "$0")" && pwd)
dir=${REVIEW_DIR:-$(git rev-parse --show-toplevel 2> /dev/null || pwd)/.review}
context="$dir/context.json"
answer="$dir/answer.json"
max_message=8000
max_comments=100

die() { echo "review.sh: $*" >&2; exit 1; }
need() { [ -f "$context" ] && [ -f "$answer" ] || die "no answer started here: run \`review.sh init CHANGE PATCHSET\` first"; }
message() {
  [ -t 0 ] && die "give the message on stdin (a heredoc, say)"
  local text
  text=$(cat)
  [ -n "${text//[[:space:]]/}" ] || die "the message is empty"
  [ "${#text}" -le "$max_message" ] || die "the message is ${#text} characters; at most $max_message"
  printf '%s' "$text"
}
resolved_flag() {
  case "${1:-}" in
    --resolved) echo true ;;
    --unresolved) echo false ;;
    "") echo "$2" ;;
    *) die "unknown flag $1: use --resolved or --unresolved" ;;
  esac
}
update() {
  local tmp
  tmp=$(mktemp "$dir/answer.XXXXXX")
  jq "$@" "$answer" > "$tmp" && mv "$tmp" "$answer"
}
room() {
  [ "$(jq '.comments | length' "$answer")" -lt "$max_comments" ] || die "the answer has $max_comments comments already, Gerrit's limit"
}

cmd=${1:-}
[ $# -gt 0 ] && shift
case "$cmd" in
  init)
    force=false
    if [ "${1:-}" = --force ]; then force=true; shift; fi
    change_arg "${1:-}" "PATCHSET [REV]"
    case "${2:-}" in "" | *[!0-9]*) die "usage: review.sh init CHANGE PATCHSET [REV]" ;; esac
    rev=${3:-}
    if [ -z "$rev" ] || ! git cat-file -e "$rev^{commit}" 2> /dev/null; then
      fetched=$("$here/fetch.sh" "$1" "$2")
      [ -z "$rev" ] || [ "$fetched" = "$(git rev-parse "$rev" 2> /dev/null || echo "$rev")" ] ||
        [ "$fetched" = "$rev" ] || die "patch set $1,$2 is $fetched, not $rev"
      rev=$fetched
    fi
    rev=$(git rev-parse "$rev^{commit}")
    mkdir -p "$dir"
    if ! $force && [ -f "$answer" ] &&
      jq -e '.review != "" or .codeReview != null or (.comments | length) > 0' "$answer" > /dev/null 2>&1; then
      die "an answer is already in progress here (\`review.sh show\`); run \`review.sh init --force ...\` to start over"
    fi
    # The files the change has at REV, with their line counts; deleted files
    # can't take a comment on this side.
    files='{}'
    while IFS= read -r path; do
      lines=$(git show "$rev:$path" 2> /dev/null | wc -l | tr -d ' ') || continue
      files=$(jq --arg p "$path" --argjson n "$lines" '. + {($p): $n}' <<< "$files")
    done < <(git diff --name-only --diff-filter=d "$rev^" "$rev")
    # Threads, numbered by when they started, so the numbers stay the same
    # as new threads are added after them.
    if ! history=$("$here/history.sh" "$1"); then
      echo "review.sh: couldn't fetch the earlier review; starting with no threads. Say so in the summary." >&2
      history='{"comments": [], "messages": [], "unavailable": true}'
    fi
    jq --argjson change "$1" --argjson patchset "$2" \
      --arg rev "$rev" --argjson files "$files" '
      # history.sh numbers the threads; group by its numbers, so there is
      # one numbering, the one every script shows.
      (.comments | group_by(.thread) | map(sort_by(.updated)) | sort_by(.[0].thread)) as $threads
      | {change: $change, patchset: $patchset, rev: $rev, files: $files,
         threads: [$threads | to_entries[] | .value as $t | ($t | last) as $l
           | {number: $t[0].thread, patch_set: $l.patch_set, path: $l.path, line: $l.line,
              started_on: $t[0].patch_set, latest: $l.id, unresolved: $l.unresolved, comments: ($t | length),
              author: $t[0].author, first: $t[0].message, last: $l.message}],
         history_unavailable: (.unavailable // false)}' <<< "$history" > "$context"
    jq -n '{review: "", codeReview: null, comments: []}' > "$answer"
    jq -r '"Started an answer to \(.change),\(.patchset) at \(.rev[0:12]): \(.files | length) files, \(.threads | length) earlier threads (`review.sh threads`)."' "$context"
    ;;

  threads)
    need
    jq -r '.threads[] | "\(.number)\t\(if .unresolved then "unresolved" else "resolved" end)\tpatch set \(.patch_set)\t\(.path)\(if .line then ":\(.line)" else "" end)\t\(.comments) comment\(if .comments == 1 then "" else "s" end), started by \(.author)\n\t\(.first | split("\n")[0] | .[0:160])\(if .comments > 1 then "\n\tlast: \(.last | split("\n")[0] | .[0:160])" else "" end)"' "$context"
    if jq -e .history_unavailable "$context" > /dev/null; then
      echo "the earlier review couldn't be fetched: review without it, and say so in the summary"
    elif [ "$(jq '.threads | length' "$context")" -eq 0 ]; then
      echo "no earlier threads: this is the change's first review"
    fi
    ;;

  reply)
    need
    case "${1:-}" in "" | *[!0-9]*) die "usage: review.sh reply N [--resolved | --unresolved] < message" ;; esac
    n=$1; shift
    thread=$(jq --argjson n "$n" '.threads[] | select(.number == $n)' "$context")
    [ -n "$thread" ] || die "no thread $n: \`review.sh threads\` lists them"
    resolved=$(resolved_flag "${1:-}" null)
    text=$(message)
    room
    update --argjson t "$thread" --arg m "$text" --argjson r "$resolved" \
      '.comments += [{path: $t.path, line: $t.line, message: $m, inReplyTo: $t.latest, patchSet: $t.patch_set, resolved: $r}]'
    echo "Replied to thread $n ($(jq -r '"\(.path)\(if .line then ":\(.line)" else "" end)"' <<< "$thread"))."
    ;;

  comment)
    need
    [ $# -ge 2 ] || die "usage: review.sh comment PATH LINE|- [--resolved | --unresolved] < message"
    path=$1 line=$2; shift 2
    lines=$(jq -r --arg p "$path" '.files[$p] // empty' "$context")
    [ -n "$lines" ] || die "$path isn't a file this patch set changes, so Gerrit won't take a comment on it: put the finding on the changed line that causes it, or in the summary. Changed: $(jq -r '.files | keys | join(", ")' "$context")"
    if [ "$line" = - ]; then
      line=null
    else
      case "$line" in "" | *[!0-9]*) die "LINE is a line number, or - for the whole file" ;; esac
      [ "$line" -ge 1 ] && [ "$line" -le "$lines" ] || die "$path has $lines lines at this patch set; line $line isn't one of them"
    fi
    resolved=$(resolved_flag "${1:-}" false)
    text=$(message)
    room
    update --arg p "$path" --argjson l "$line" --arg m "$text" --argjson r "$resolved" \
      '.comments += [{path: $p, line: $l, message: $m, inReplyTo: null, patchSet: null, resolved: $r}]'
    echo "Commented on $path$( [ "$line" = null ] || echo ":$line")."
    ;;

  drop)
    need
    case "${1:-}" in "" | *[!0-9]*) die "usage: review.sh drop K" ;; esac
    [ "$1" -ge 1 ] && [ "$1" -le "$(jq '.comments | length' "$answer")" ] || die "no comment $1: \`review.sh show\` numbers them"
    update --argjson k "$1" 'del(.comments[$k - 1])'
    echo "Dropped comment $1."
    ;;

  vote)
    need
    case "${1:-}" in -1 | 0 | 1) ;; *) die "the vote is -1, 0 or 1" ;; esac
    update --argjson v "$1" '.codeReview = $v'
    echo "Vote: $1."
    ;;

  summary)
    need
    text=$(message)
    update --arg s "$text" '.review = $s'
    echo "Summary set ($(printf '%s\n' "$text" | wc -l | tr -d ' ') lines)."
    ;;

  show)
    need
    jq -r '"Vote: \(.codeReview // "not set")\n\nSummary:\n\(if .review == "" then "(not written)" else .review end)\n\nComments (\(.comments | length)):",
      (.comments | to_entries[] | .key as $k | .value
       | "\($k + 1). \(if .inReplyTo then "reply on patch set \(.patchSet)," else "new," end) \(.path)\(if .line then ":\(.line)" else "" end)\(if .resolved == true then ", resolved" elif .resolved == false then ", unresolved" else "" end)\n   \(.message | gsub("\n"; "\n   "))")' "$answer"
    ;;

  check)
    file=${1:-$answer}
    [ -f "$file" ] || die "no answer at $file"
    jq -e --argjson max "$max_comments" --argjson len "$max_message" '
      (.review | type == "string" and length > 0)
      and (.codeReview | IN(-1, 0, 1))
      and (.comments | type == "array" and length <= $max)
      and all(.comments[];
        (.path | type == "string" and length > 0)
        and (.line == null or (.line | type == "number" and . >= 1 and floor == .))
        and (.message | type == "string" and length > 0 and length <= $len)
        and ((.inReplyTo == null and .patchSet == null)
             or ((.inReplyTo | type == "string" and test("^[0-9a-f]{8}_[0-9a-f]{8}$"))
                 and (.patchSet | type == "number" and . >= 1)))
        and (.resolved == null or (.resolved | type == "boolean")))' "$file" > /dev/null ||
      die "$file isn't a complete answer: it needs a summary (\`review.sh summary\`), a vote (\`review.sh vote\`) and well-formed comments"
    echo "The answer is complete: vote $(jq '.codeReview' "$file"), $(jq '.comments | length' "$file") comments."
    echo "Nothing more to do: there is no submit step. The review job posts this file as it is."
    ;;

  "" | -h | --help | help)
    sed -n '/^# Builds/,/^[^#]/p' "$0" | grep '^#' | grep -v '^# shellcheck' | sed 's/^# \{0,1\}//'
    ;;
  *)
    die "unknown command $cmd: run \`review.sh help\`"
    ;;
esac
