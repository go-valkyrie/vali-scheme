#!/usr/bin/env python3
"""Render a Muse Code session export as a page to read in a browser.

    render-session.py EXPORT.json [REVIEW.json] --html OUT.html [--summary OUT.md]

EXPORT.json is what `muse export --session ID --out EXPORT.json` writes;
REVIEW.json, if given, is the review the job posts. The HTML page is
self-contained: the prompt, each step's reasoning summary, messages, tool
calls with their arguments and results (folded), token use, and the review.
The summary is a short Markdown version for the job's step summary.

Standard library only, so the review job needs nothing installed for it.
"""

import argparse
import html
import json
import sys
from collections import OrderedDict

# Long tool results are cut to this many characters in the page; the export
# itself keeps them whole.
RESULT_LIMIT = 20_000


def events(export):
    """The session's events, in order: (payload_type, event or payload)."""
    for record in export.get("events", []):
        envelope = record.get("envelope", {})
        payload = envelope.get("payload") or {}
        yield envelope.get("payload_type", ""), payload.get("event") or payload


def prompt_text(export):
    for kind, payload in events(export):
        if kind == "runtime.user_intent.materialized":
            parts = []
            for message in payload.get("model_messages", []):
                for block in message.get("content", []):
                    if block.get("kind") == "text":
                        parts.append(block.get("text", ""))
            return "\n".join(parts)
    return ""


def steps(export):
    """Groups the events into model steps, each a dict of what happened."""
    out = []
    step = None
    reasoning = OrderedDict()
    results = {}

    def new_step():
        nonlocal step
        step = {"reasoning": [], "messages": [], "calls": [], "usage": None}
        out.append(step)

    for _, ev in events(export):
        kind = ev.get("kind")
        if kind == "model_response_created" or step is None:
            if kind == "model_response_created" or step is None:
                new_step()
        if kind == "reasoning_summary_delta":
            key = ev.get("message_id")
            reasoning.setdefault(key, [])
            reasoning[key].append(ev.get("text", ""))
            if key not in step["reasoning"]:
                step["reasoning"].append(key)
        elif kind == "assistant_message_committed" and ev.get("text"):
            step["messages"].append((ev.get("phase") or "message", ev["text"]))
        elif kind == "assistant_tool_calls_committed":
            for call in ev.get("tool_calls", []):
                step["calls"].append(call)
        elif kind == "tool_result_batch_committed":
            for result in ev.get("results", []):
                results[result.get("tool_call_id")] = result.get("text", "")
        elif kind == "model_completed":
            step["usage"] = ev
    for s in out:
        s["reasoning"] = ["".join(reasoning[k]) for k in s["reasoning"]]
        for call in s["calls"]:
            call["result"] = results.get(call.get("call_id"))
    return [s for s in out if s["reasoning"] or s["messages"] or s["calls"] or s["usage"]]


def outcome(export):
    for _, ev in events(export):
        if ev.get("kind") == "terminal":
            return ev
    return {}


def usage_totals(all_steps):
    totals = {"input_tokens": 0, "output_tokens": 0, "reasoning_tokens": 0}
    model = None
    for s in all_steps:
        u = s["usage"] or {}
        model = u.get("model") or model
        for key in totals:
            totals[key] += (u.get("usage") or {}).get(key) or 0
    return model, totals


def pretty_args(args):
    try:
        return json.dumps(json.loads(args), indent=2, ensure_ascii=False)
    except (TypeError, ValueError):
        return str(args)


def cut(text, limit=RESULT_LIMIT):
    if text is None:
        return "(no result recorded)"
    if len(text) <= limit:
        return text
    return text[:limit] + f"\n… ({len(text) - limit} more characters in the export)"


def e(text):
    return html.escape(text or "")


STYLE = """
:root { --bg: #fff; --fg: #1f2328; --muted: #59636e; --line: #d1d9e0;
        --box: #f6f8fa; --accent: #0969da; --bad: #cf222e; --good: #1a7f37; }
@media (prefers-color-scheme: dark) {
  :root { --bg: #0d1117; --fg: #e6edf3; --muted: #9198a1; --line: #3d444d;
          --box: #151b23; --accent: #4493f8; --bad: #f85149; --good: #3fb950; } }
body { background: var(--bg); color: var(--fg); margin: 0 auto; max-width: 980px;
       padding: 16px; font: 15px/1.5 -apple-system, system-ui, sans-serif; }
h1 { font-size: 1.4em; } h2 { font-size: 1.15em; border-bottom: 1px solid var(--line); }
pre { background: var(--box); border: 1px solid var(--line); border-radius: 6px;
      padding: 10px; overflow-x: auto; white-space: pre-wrap; word-break: break-word;
      font: 12.5px/1.45 ui-monospace, SFMono-Regular, Menlo, monospace; }
details { border: 1px solid var(--line); border-radius: 6px; margin: 6px 0; padding: 4px 10px; }
summary { cursor: pointer; color: var(--accent); }
.step { border-left: 3px solid var(--line); padding-left: 12px; margin: 18px 0; }
.meta { color: var(--muted); font-size: 0.9em; }
.reason { color: var(--muted); font-style: italic; }
.vote-1 { color: var(--good); } .vote--1 { color: var(--bad); }
"""


def render_html(export, review):
    all_steps = steps(export)
    model, totals = usage_totals(all_steps)
    end = outcome(export)
    out = ["<!doctype html><html lang=en><meta charset=utf-8>",
           "<meta name=viewport content='width=device-width, initial-scale=1'>",
           "<title>Muse review session</title>", f"<style>{STYLE}</style><body>",
           "<h1>Muse review session</h1>",
           f"<p class=meta>Model {e(model or 'unknown')} · {len(all_steps)} steps · "
           f"{totals['input_tokens']:,} input, {totals['output_tokens']:,} output "
           f"({totals['reasoning_tokens']:,} reasoning) tokens · ended "
           f"{e(str(end.get('terminal', 'unknown')))}"
           + (f" in {end['turn_duration_ms'] / 1000:.0f} s" if end.get("turn_duration_ms") else "")
           + "</p>"]
    if review is not None:
        vote = review.get("codeReview")
        out.append("<h2>Review</h2>")
        out.append(f"<p>Code-Review <b class='vote-{e(str(vote))}'>{e(f'{vote:+d}' if isinstance(vote, int) else str(vote))}</b></p>")
        out.append(f"<pre>{e(review.get('review', ''))}</pre>")
        for c in review.get("comments", []):
            where = c.get("path", "?") + (f":{c['line']}" if c.get("line") else "")
            out.append(f"<details open><summary>{e(where)}</summary><pre>{e(c.get('message', ''))}</pre></details>")
    out.append("<h2>Prompt</h2>")
    out.append(f"<details><summary>The prompt Muse was given</summary><pre>{e(prompt_text(export))}</pre></details>")
    out.append("<h2>Steps</h2>")
    for n, s in enumerate(all_steps, 1):
        u = s["usage"] or {}
        tokens = (u.get("usage") or {})
        out.append(f"<div class=step><p class=meta>Step {n}"
                   + (f" · {tokens.get('input_tokens', 0):,} in / {tokens.get('output_tokens', 0):,} out"
                      f" · {u.get('duration_ms', 0) / 1000:.1f} s · {e(u.get('finish_reason', ''))}" if u else "")
                   + "</p>")
        for text in s["reasoning"]:
            out.append(f"<p class=reason>{e(text)}</p>")
        for phase, text in s["messages"]:
            out.append(f"<p class=meta>{e(phase)}</p><pre>{e(text)}</pre>")
        for call in s["calls"]:
            out.append(f"<details><summary><code>{e(call.get('name', '?'))}</code> "
                       f"{e(short_args(call.get('args')))}</summary>"
                       f"<p class=meta>Arguments</p><pre>{e(pretty_args(call.get('args')))}</pre>"
                       f"<p class=meta>Result</p><pre>{e(cut(call.get('result')))}</pre></details>")
        out.append("</div>")
    out.append("</body></html>")
    return "\n".join(out)


def short_args(args):
    try:
        value = json.loads(args)
    except (TypeError, ValueError):
        return ""
    if isinstance(value, dict):
        for key in ("path", "pattern", "query", "command"):
            if key in value:
                return str(value[key])[:120]
    return ""


def render_summary(export, review):
    all_steps = steps(export)
    model, totals = usage_totals(all_steps)
    calls = sum(len(s["calls"]) for s in all_steps)
    lines = ["### Muse review",
             f"{model or 'unknown model'}: {len(all_steps)} steps, {calls} tool calls, "
             f"{totals['input_tokens']:,} input and {totals['output_tokens']:,} output tokens. "
             "The full session is in the `muse-session` artifact (open `session.html`)."]
    if review is not None:
        vote = review.get("codeReview")
        lines += ["", f"**Code-Review {vote:+d}**" if isinstance(vote, int) else "", "",
                  review.get("review", "")]
        if review.get("comments"):
            lines += ["", f"{len(review['comments'])} inline comments:"]
            lines += [f"- `{c.get('path')}{':' + str(c['line']) if c.get('line') else ''}`" for c in review["comments"]]
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("export")
    parser.add_argument("review", nargs="?")
    parser.add_argument("--html", required=True)
    parser.add_argument("--summary")
    args = parser.parse_args()
    with open(args.export, encoding="utf-8") as f:
        export = json.load(f)
    review = None
    if args.review:
        try:
            with open(args.review, encoding="utf-8") as f:
                review = json.load(f)
        except (OSError, ValueError):
            review = None
    with open(args.html, "w", encoding="utf-8") as f:
        f.write(render_html(export, review))
    if args.summary:
        with open(args.summary, "w", encoding="utf-8") as f:
            f.write(render_summary(export, review))
    return 0


if __name__ == "__main__":
    sys.exit(main())
