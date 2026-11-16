#!/bin/bash
# Session start hook.
#
# In Claude Code cloud sessions, first installs mise (through the pinned
# bin/mise wrapper) and the tools in mise.toml, and puts them on the
# session's PATH. Then, everywhere, hands over to jouet's own session-start
# hook, so jouet only ever runs after it has been installed.
set -euo pipefail

project_dir="${CLAUDE_PROJECT_DIR:-$PWD}"
cd "$project_dir"

if [ "${CLAUDE_CODE_REMOTE:-}" = "true" ]; then
  mise_data_dir="${MISE_DATA_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/mise}"

  # Install only when something that decides what gets installed has changed
  # since the last successful install. The hash file lives in mise's data
  # directory, so it disappears along with the installed tools.
  hashes="$mise_data_dir/.vali-session-start.sha256"
  if ! sha256sum --check --status "$hashes" 2>/dev/null; then
    rm -f "$hashes"
    # Downloads the pinned mise on first use, then installs mise.toml's
    # tools. Its output goes to stderr, because this hook's stdout is what
    # Claude reads, and it gets no stdin, which carries the hook input for
    # jouet.
    bin/mise install </dev/null >&2
    sha256sum .claude/hooks/session-start.sh bin/mise mise.toml rust-toolchain.toml \
      > "$hashes" || rm -f "$hashes"
  fi

  # Commands in the session get bin/ (for `mise`, the pinned wrapper) and
  # mise's shims (for jouet, cargo-deny and the Rust toolchain).
  if [ -n "${CLAUDE_ENV_FILE:-}" ]; then
    echo "export PATH=\"$project_dir/bin:$mise_data_dir/shims:\$PATH\"" >> "$CLAUDE_ENV_FILE"
  fi
fi

# jouet reads the same hook input from stdin and writes what Claude sees.
exec bin/mise -C "$project_dir" exec -- jouet hook claude session-start
