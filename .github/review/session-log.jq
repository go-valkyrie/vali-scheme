# Turns a Muse Code session log (session.jsonl) into lines to read as it
# runs: Muse's messages, its reasoning summaries, each tool call in full,
# each model call's token use, and the end of each turn. watch-session.sh
# feeds it, with `--arg prefix` marking whose session each line is from: a
# workflow subagent's id for its own, nothing for the session itself, as
# when the argument is left out.
(.payload.event // .payload // {}) as $e
| ($e.kind // "") as $k
| if $k == "reasoning_summary_delta" then
    "  … " + $e.text
  elif $k == "assistant_message_committed" and ($e.text // "") != "" then
    "» " + ($e.text | gsub("\n"; "\n  "))
  elif $k == "assistant_tool_calls_committed" then
    $e.tool_calls[]
    | "▶ " + .name + " "
      + ((.args | fromjson? // .)
         | if type == "object" then (.command // .path // .name // tostring) else tostring end
         | tostring
         | gsub("\n"; "\n    "))
  elif $k == "model_completed" then
    "· model: \($e.usage.input_tokens) in / \($e.usage.output_tokens) out, \(($e.duration_ms // 0) / 1000 | floor)s"
  elif $k == "terminal" then
    # The end of a turn, not of the session: after starting a workflow the
    # session ends its turn and waits, and its subagents' lines follow.
    "■ turn \($e.terminal // "ended") after \(($e.turn_duration_ms // 0) / 1000 | floor)s"
  else
    empty
  end
| ($ARGS.named.prefix // "") + .
