#!/usr/bin/env bash
# Installs the newest bazel-tui-agent release and enables it in ~/.bazelrc:
#
#   curl -fsSL https://raw.githubusercontent.com/bluecmd/bazel-tui-agent/main/install.sh | bash
#
# The jar goes to ~/.local/share/bazel-tui-agent/bazel-tui-agent.jar and one startup line is
# appended to ~/.bazelrc (not again if it is already there). Running it again updates the jar in
# place. Settings, as environment variables:
#   VERSION=v0.3            a release tag (default: the newest release, pre-releases included)
#   DIR=/some/dir           where the jar goes
#   BAZELRC_FILE=/path      which bazelrc gets the startup line
set -euo pipefail

REPO=bluecmd/bazel-tui-agent
DIR=${DIR:-$HOME/.local/share/bazel-tui-agent}
RC=${BAZELRC_FILE:-$HOME/.bazelrc}
JAR="$DIR/bazel-tui-agent.jar"

command -v curl >/dev/null || { echo "install.sh: curl is needed" >&2; exit 1; }

# Newest release including pre-releases (the API lists them newest first); the releases/latest
# redirect skips pre-releases, so it is only the fallback when the API is unreachable.
VERSION=${VERSION:-}
if [ -z "$VERSION" ]; then
  VERSION=$(curl -fsSL "https://api.github.com/repos/$REPO/releases?per_page=1" 2>/dev/null |
    sed -n 's/^ *"tag_name": *"\([^"]*\)".*/\1/p' | head -1 || true)
fi
if [ -n "$VERSION" ]; then
  URL="https://github.com/$REPO/releases/download/$VERSION/bazel-tui-agent.jar"
else
  URL="https://github.com/$REPO/releases/latest/download/bazel-tui-agent.jar"
fi

mkdir -p "$DIR"
had_jar=0
[ -f "$JAR" ] && had_jar=1
echo "downloading ${VERSION:-the latest release} ..."
curl -fsSL --retry 3 -o "$JAR.tmp" "$URL"
# A jar is a zip; anything else (an error page, say) is not installed.
[ "$(head -c 2 "$JAR.tmp")" = PK ] || { rm -f "$JAR.tmp"; echo "install.sh: $URL is not a jar" >&2; exit 1; }
# mv keeps a running Bazel server on the old file until it restarts.
mv -f "$JAR.tmp" "$JAR"

if command -v unzip >/dev/null; then
  built=$(unzip -p "$JAR" bazelhook/version 2>/dev/null || true)
  [ -n "$built" ] && echo "installed $built to $JAR" || echo "installed to $JAR"
else
  echo "installed to $JAR"
fi

LINE="startup --host_jvm_args=-javaagent:$JAR"
if [ -f "$RC" ] && grep -qF -- "$LINE" "$RC"; then
  echo "$RC already enables it"
  if [ "$had_jar" = 1 ]; then
    echo "a running Bazel server keeps the old jar: run 'bazel shutdown' in that workspace"
  fi
elif [ -f "$RC" ] && grep -q 'javaagent:.*bazel-tui-agent' "$RC"; then
  echo "$RC enables a bazel-tui-agent jar from another path; point it at $JAR:"
  echo "  $LINE"
else
  printf '%s\n' "$LINE" >>"$RC"
  echo "added to $RC: $LINE"
  echo "the next bazel command in a terminal shows the UI (the server restarts once)"
fi
