#!/usr/bin/env python3
"""Render a Muse Code session export as a page to read in a browser.

    render-session.py EXPORT.json [REVIEW.json] --html OUT.html [--summary OUT.md]

EXPORT.json is what `muse export --session ID --out EXPORT.json` writes;
REVIEW.json, if given, is the review the job posts. The HTML page is
self-contained: a header with the model, tokens, time and vote; the review
and its inline comments; then a timeline of the session, with Muse's
reasoning and messages, and each model call and tool call as a row that
opens to its details. The prompt and the diff are folded away. The summary
is a short Markdown version for the job's step summary.

Standard library only, so the review job needs nothing installed for it.
"""

import argparse
import html
import json
import re
import sys

# A tool result shows this many lines before "show all"; past FULL_LIMIT
# characters even "show all" is cut, and the export keeps the rest.
PREVIEW_LINES = 30
FULL_LIMIT = 40_000


# --- reading the export -------------------------------------------------------


def records(export):
    """Each event as (time in seconds, payload_type, event or payload)."""
    for record in export.get("events", []):
        envelope = record.get("envelope", {})
        payload = envelope.get("payload") or {}
        at = envelope.get("recorded_at") or record.get("recorded_at")
        yield (at / 1e6 if at else None), envelope.get("payload_type", ""), payload.get("event") or payload


def prompt_text(export):
    # The accepted intent carries the prompt's text; the materialized one
    # only refers back to it.
    for _, kind, payload in records(export):
        if kind.startswith("runtime.user_intent.") and payload.get("model_messages"):
            return "\n".join(
                block.get("text", "")
                for message in payload.get("model_messages", [])
                for block in message.get("content", [])
                if block.get("kind") == "text"
            )
    return ""


def timeline(export):
    """The session as a list of entries in time order."""
    entries = []
    reasoning = {}
    calls = {}
    start = None
    for at, _, ev in records(export):
        if at is not None and start is None:
            start = at
        kind = ev.get("kind")
        if kind == "reasoning_summary_delta":
            key = ev.get("message_id")
            if key not in reasoning:
                reasoning[key] = {"type": "reasoning", "at": at, "text": ""}
                entries.append(reasoning[key])
            reasoning[key]["text"] += ev.get("text", "")
        elif kind == "assistant_message_committed" and ev.get("text"):
            entries.append({"type": "message", "at": at, "phase": ev.get("phase") or "", "text": ev["text"]})
        elif kind == "assistant_tool_calls_committed":
            for call in ev.get("tool_calls", []):
                entry = {"type": "tool", "at": at, "name": call.get("name", "?"),
                         "args": call.get("args"), "result": None, "done": None}
                calls[call.get("call_id")] = entry
                entries.append(entry)
        elif kind == "tool_result_batch_committed":
            for result in ev.get("results", []):
                entry = calls.get(result.get("tool_call_id"))
                if entry is not None:
                    entry["result"] = result.get("text", "")
                    entry["done"] = at
        elif kind == "model_completed":
            entries.append({"type": "model", "at": at, "usage": ev.get("usage") or {},
                            "duration_ms": ev.get("duration_ms"), "finish": ev.get("finish_reason"),
                            "model": ev.get("model")})
    # Reasoning that says nothing is noise.
    entries = [x for x in entries if not (x["type"] == "reasoning" and not x["text"].strip())]
    return start, entries


def outcome(export):
    for _, _, ev in records(export):
        if ev.get("kind") == "terminal":
            return ev
    return {}


def subagent_usage(session_dir):
    """Token use of the workflow subagents a session started, which its export
    leaves out: each is a session of its own under the session's directory."""
    import glob, os
    sums = {"input_tokens": 0, "output_tokens": 0, "reasoning_tokens": 0}
    agents = calls = 0
    if not session_dir:
        return sums, agents, calls

    def walk(o):
        if isinstance(o, dict):
            if o.get("kind") == "model_completed" and isinstance(o.get("usage"), dict):
                yield o
            for v in o.values():
                yield from walk(v)
        elif isinstance(o, list):
            for v in o:
                yield from walk(v)

    for sub in sorted(glob.glob(os.path.join(session_dir, "subagent", "*"))):
        agents += 1
        seen = set()
        for path in glob.glob(os.path.join(sub, "**", "*.jsonl"), recursive=True):
            with open(path, encoding="utf-8", errors="replace") as f:
                for line in f:
                    try:
                        record = json.loads(line)
                    except ValueError:
                        continue
                    for mc in walk(record):
                        key = json.dumps(mc, sort_keys=True)
                        if key in seen:
                            continue
                        seen.add(key)
                        calls += 1
                        for k in sums:
                            sums[k] += mc["usage"].get(k) or 0
    return sums, agents, calls


def totals(entries):
    model = None
    sums = {"input_tokens": 0, "output_tokens": 0, "reasoning_tokens": 0}
    for x in entries:
        if x["type"] == "model":
            model = x.get("model") or model
            for key in sums:
                sums[key] += x["usage"].get(key) or 0
    return model, sums


# --- a small Markdown renderer --------------------------------------------------
# Enough for reviews and model messages: headings, paragraphs, lists, fenced
# and inline code, bold, italics, links. Everything is escaped first.


def inline(text):
    parts = re.split(r"(`[^`]+`)", text)
    out = []
    for part in parts:
        if part.startswith("`") and part.endswith("`") and len(part) > 1:
            out.append(f"<code>{html.escape(part[1:-1])}</code>")
            continue
        s = html.escape(part)
        s = re.sub(r"\[([^\]]+)\]\((https?://[^)\s]+)\)", r'<a href="\2">\1</a>', s)
        s = re.sub(r"\*\*(.+?)\*\*", r"<strong>\1</strong>", s)
        s = re.sub(r"(?<![\w*])\*(?!\s)(.+?)(?<!\s)\*(?![\w*])", r"<em>\1</em>", s)
        out.append(s)
    return "".join(out)


def markdown(text):
    # HTML comments are notes for whoever edits the file, as Markdown hides them.
    text = re.sub(r"<!--.*?-->", "", text or "", flags=re.S)
    lines = text.replace("\r\n", "\n").split("\n")
    out = []
    para = []
    lists = []  # stack of (indent, tag)

    def flush_para():
        if para:
            out.append(f"<p>{inline(' '.join(para))}</p>")
            para.clear()

    def close_lists(indent=-1):
        while lists and lists[-1][0] > indent:
            out.append(f"</li></{lists.pop()[1]}>")

    i = 0
    while i < len(lines):
        line = lines[i]
        fence = re.match(r"^\s*(`{3,}|~{3,})(.*)$", line)
        if fence:
            flush_para()
            close_lists()
            mark, lang = fence.group(1), fence.group(2).strip()
            body = []
            i += 1
            while i < len(lines) and not lines[i].strip().startswith(mark):
                body.append(lines[i])
                i += 1
            out.append(code_block("\n".join(body), lang))
            i += 1
            continue
        heading = re.match(r"^(#{1,6})\s+(.*)$", line)
        item = re.match(r"^(\s*)([-*+]|\d+[.)])\s+(.*)$", line)
        if heading:
            flush_para()
            close_lists()
            level = min(len(heading.group(1)) + 2, 6)
            out.append(f"<h{level}>{inline(heading.group(2))}</h{level}>")
        elif item:
            flush_para()
            indent = len(item.group(1))
            tag = "ol" if item.group(2)[0].isdigit() else "ul"
            if lists and lists[-1][0] == indent:
                out.append("</li><li>")
            elif lists and lists[-1][0] > indent:
                close_lists(indent)
                out.append("</li><li>" if lists else f"<{tag}><li>")
                if not lists:
                    lists.append((indent, tag))
            else:
                out.append(f"<{tag}><li>")
                lists.append((indent, tag))
            out.append(inline(item.group(3)))
        elif not line.strip():
            flush_para()
        elif lists and line.startswith(" "):
            out.append(" " + inline(line.strip()))
        else:
            close_lists()
            para.append(line.strip())
        i += 1
    flush_para()
    close_lists()
    return "\n".join(out)


def code_block(body, lang=""):
    if lang == "diff" or (not lang and re.search(r"^(\+\+\+|---|@@) ", body, re.M)):
        rows = []
        for row in body.split("\n"):
            cls = ("add" if row.startswith("+") and not row.startswith("+++")
                   else "del" if row.startswith("-") and not row.startswith("---")
                   else "hunk" if row.startswith("@@") else "")
            rows.append(f'<span class="{cls}">{html.escape(row)}</span>' if cls else html.escape(row))
        return "<pre class=diff>" + "\n".join(rows) + "</pre>"
    return f"<pre>{html.escape(body)}</pre>"


# --- the page -------------------------------------------------------------------


def e(text):
    return html.escape(str(text) if text is not None else "")


def seconds(value):
    if value is None:
        return ""
    return f"{value:.1f} s" if value < 60 else f"{int(value // 60)} m {int(value % 60)} s"


def clock(at, start):
    return f"+{at - start:5.1f}s" if at is not None and start is not None else ""


def args_of(raw):
    try:
        value = json.loads(raw)
    except (TypeError, ValueError):
        return raw, None
    return value, json.dumps(value, indent=2, ensure_ascii=False)


def target(value):
    if isinstance(value, dict):
        for key in ("path", "pattern", "query", "command", "url", "name"):
            if key in value:
                extra = ""
                if "start_line" in value or "end_line" in value:
                    extra = f":{value.get('start_line', '')}-{value.get('end_line', '')}"
                return f"{value[key]}{extra}"
    return ""


def result_block(text):
    if text is None:
        return "<p class=meta>No result recorded.</p>"
    lines = text.split("\n")
    size = f"{len(lines):,} lines, {len(text):,} characters"
    if len(lines) <= PREVIEW_LINES:
        return f"<p class=meta>{size}</p><pre>{e(text)}</pre>"
    preview = "\n".join(lines[:PREVIEW_LINES])
    full = text if len(text) <= FULL_LIMIT else text[:FULL_LIMIT] + "\n… (cut; the rest is in session.json)"
    return (f"<p class=meta>{size}; the first {PREVIEW_LINES} lines:</p><pre>{e(preview)}</pre>"
            f"<details class=more><summary>Show all</summary><pre>{e(full)}</pre></details>")


def split_prompt(text):
    """The prompt's own instructions, and the change details and diff the job appended."""
    m = re.search(r"^Change \d+, patch set \d+", text, re.M)
    if not m:
        return text, ""
    return text[: m.start()].strip(), text[m.start():].strip()


STYLE = """
:root { --bg:#fff; --fg:#1f2328; --muted:#59636e; --line:#d1d9e0; --box:#f6f8fa;
        --accent:#0969da; --bad:#cf222e; --good:#1a7f37; --add:#dafbe1; --del:#ffebe9; --hunk:#ddf4ff; }
@media (prefers-color-scheme: dark) {
  :root { --bg:#0d1117; --fg:#e6edf3; --muted:#9198a1; --line:#3d444d; --box:#151b23;
          --accent:#4493f8; --bad:#f85149; --good:#3fb950; --add:#033a16; --del:#4f1b1b; --hunk:#0c2d6b; } }
* { box-sizing: border-box; }
body { background:var(--bg); color:var(--fg); margin:0; font:15px/1.55 -apple-system, system-ui, sans-serif; }
header { position:sticky; top:0; z-index:1; background:var(--bg); border-bottom:1px solid var(--line);
         padding:10px 16px; display:flex; gap:18px; flex-wrap:wrap; align-items:baseline; }
header h1 { font-size:1.05em; margin:0; }
main { max-width:980px; margin:0 auto; padding:8px 16px 40px; }
h2 { font-size:1.1em; border-bottom:1px solid var(--line); padding-bottom:4px; margin-top:28px; }
h3, h4, h5, h6 { font-size:1em; }
pre { background:var(--box); border:1px solid var(--line); border-radius:6px; padding:10px; overflow-x:auto;
      white-space:pre-wrap; word-break:break-word; font:12.5px/1.45 ui-monospace, SFMono-Regular, Menlo, monospace; }
code { font:0.9em ui-monospace, SFMono-Regular, Menlo, monospace; background:var(--box); padding:1px 4px; border-radius:4px; }
pre code { background:none; padding:0; }
.diff .add { background:var(--add); display:block; } .diff .del { background:var(--del); display:block; }
.diff .hunk { background:var(--hunk); display:block; }
details { border:1px solid var(--line); border-radius:6px; margin:6px 0; padding:2px 10px; }
details > summary { cursor:pointer; padding:4px 0; }
details.more { border:none; padding:0; } details.more > summary { color:var(--accent); font-size:0.9em; }
.meta { color:var(--muted); font-size:0.88em; margin:4px 0; }
.vote { font-weight:600; } .vote.good { color:var(--good); } .vote.bad { color:var(--bad); }
.comment { border-left:3px solid var(--accent); padding:2px 12px; margin:10px 0; }
.comment .where { font:0.9em ui-monospace, Menlo, monospace; color:var(--muted); }
.feed { list-style:none; padding:0; margin:0; }
.feed > li { display:grid; grid-template-columns:4.5em 1fr; gap:8px; padding:6px 0; border-bottom:1px solid var(--line); }
.feed .t { color:var(--muted); font:12px ui-monospace, Menlo, monospace; padding-top:3px; }
.feed .reasoning { color:var(--muted); font-style:italic; }
.feed .model { color:var(--muted); font-size:0.88em; }
.feed details { margin:0; }
.tool summary code { background:none; padding:0; font-weight:600; }
.tool .target { font:0.88em ui-monospace, Menlo, monospace; }
"""


def vote_html(vote):
    if not isinstance(vote, int):
        return '<span class="vote">no vote</span>'
    cls = "good" if vote > 0 else "bad" if vote < 0 else ""
    return f'<span class="vote {cls}">Code-Review {vote:+d}</span>' if vote else '<span class="vote">Code-Review 0</span>'


def render_html(export, review, session_dir=None):
    start, entries = timeline(export)
    model, sums = totals(entries)
    sub, agents, sub_calls = subagent_usage(session_dir)
    for k in sums:
        sums[k] += sub[k]
    with_agents = f" · incl. {agents} subagents ({sub_calls} model calls)" if agents else ""
    end = outcome(export)
    tool_count = sum(1 for x in entries if x["type"] == "tool")
    duration = (end.get("turn_duration_ms") or 0) / 1000 or None
    out = ["<!doctype html><html lang=en><meta charset=utf-8>",
           "<meta name=viewport content='width=device-width, initial-scale=1'>",
           "<title>Muse review session</title>", f"<style>{STYLE}</style><body>",
           "<header><h1>Muse review session</h1>",
           f"<span>{vote_html(review.get('codeReview')) if review else 'no review'}</span>",
           f"<span class=meta>{e(model or 'unknown model')}</span>",
           f"<span class=meta>{sums['input_tokens']:,} in · {sums['output_tokens']:,} out "
           f"({sums['reasoning_tokens']:,} reasoning){with_agents}</span>",
           f"<span class=meta>{tool_count} tool calls · {seconds(duration)} · "
           f"{e(end.get('terminal', 'unfinished'))}</span></header><main>"]

    if review:
        out.append("<h2>Review</h2>")
        out.append(markdown(review.get("review", "")))
        comments = review.get("comments") or []
        new = [c for c in comments if not c.get("inReplyTo")]
        replies = [c for c in comments if c.get("inReplyTo")]
        for title, group in ((f"Replies to earlier threads ({len(replies)})", replies),
                             (f"Inline comments ({len(new)})", new)):
            if not group:
                continue
            out.append(f"<h2>{title}</h2>")
            for c in group:
                where = c.get("path", "?") + (f":{c['line']}" if c.get("line") else " (whole file)")
                if c.get("inReplyTo"):
                    state = {True: "resolves the thread", False: "thread still open", None: "thread left as it was"}
                    where += f" · patch set {c.get('patchSet', '?')} · {state.get(c.get('resolved'), '')}"
                out.append(f"<div class=comment><div class=where>{e(where)}</div>{markdown(c.get('message', ''))}</div>")

    instructions, appended = split_prompt(prompt_text(export))
    out.append("<h2>Prompt</h2>")
    out.append(f"<details><summary>Instructions</summary>{markdown(instructions)}</details>")
    if appended:
        out.append(f"<details><summary>The change and its diff</summary>{markdown(appended)}</details>")

    out.append("<h2>Timeline</h2><ol class=feed>")
    for x in entries:
        out.append(f"<li><span class=t>{clock(x['at'], start)}</span><div>")
        if x["type"] == "reasoning":
            out.append(f"<div class=reasoning>{inline(x['text'])}</div>")
        elif x["type"] == "message":
            label = "" if x["phase"] in ("", "final") else f"<p class=meta>{e(x['phase'])}</p>"
            out.append(label + markdown(x["text"]))
        elif x["type"] == "model":
            u = x["usage"]
            out.append(f"<div class=model>model call · {u.get('input_tokens', 0):,} in / "
                       f"{u.get('output_tokens', 0):,} out · {seconds((x.get('duration_ms') or 0) / 1000)}"
                       f" · {e(x.get('finish') or '')}</div>")
        elif x["type"] == "tool":
            value, pretty = args_of(x["args"])
            took = seconds(x["done"] - x["at"]) if x["done"] and x["at"] else ""
            size = f"{len((x['result'] or '').splitlines()):,} lines" if x["result"] is not None else "no result"
            out.append(f"<details class=tool><summary><code>{e(x['name'])}</code> "
                       f"<span class=target>{e(target(value))}</span> "
                       f"<span class=meta>· {size}{' · ' + took if took else ''}</span></summary>")
            if pretty and not (isinstance(value, dict) and len(value) == 1 and target(value)):
                out.append(f"<p class=meta>Arguments</p><pre>{e(pretty)}</pre>")
            out.append(result_block(x["result"]))
            out.append("</details>")
        out.append("</div></li>")
    out.append("</ol></main></body></html>")
    return "\n".join(out)


def render_summary(export, review, session_dir=None):
    _, entries = timeline(export)
    model, sums = totals(entries)
    sub, agents, _ = subagent_usage(session_dir)
    for k in sums:
        sums[k] += sub[k]
    tool_count = sum(1 for x in entries if x["type"] == "tool")
    lines = ["### Muse review",
             f"{model or 'unknown model'}: {tool_count} tool calls, {sums['input_tokens']:,} input and "
             f"{sums['output_tokens']:,} output tokens{f' (with {agents} subagents)' if agents else ''}. The whole session is `session.html` in the "
             "`muse-session` artifact."]
    if review:
        vote = review.get("codeReview")
        lines += ["", f"**Code-Review {vote:+d}**" if isinstance(vote, int) else "**No vote**", "",
                  review.get("review", "")]
        comments = review.get("comments") or []
        replies = [c for c in comments if c.get("inReplyTo")]
        new = [c for c in comments if not c.get("inReplyTo")]
        if replies:
            settled = sum(1 for c in replies if c.get("resolved") is True)
            lines += ["", f"{len(replies)} replies to earlier threads, {settled} resolving them."]
        if new:
            lines += ["", f"{len(new)} inline comments:"]
            lines += [f"- `{c.get('path')}{':' + str(c['line']) if c.get('line') else ''}`" for c in new]
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("export")
    parser.add_argument("review", nargs="?")
    parser.add_argument("--html", required=True)
    parser.add_argument("--summary")
    parser.add_argument("--session-dir", help="the session's directory, to count its subagents' tokens")
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
        f.write(render_html(export, review, args.session_dir))
    if args.summary:
        with open(args.summary, "w", encoding="utf-8") as f:
            f.write(render_summary(export, review, args.session_dir))
    return 0


if __name__ == "__main__":
    sys.exit(main())
