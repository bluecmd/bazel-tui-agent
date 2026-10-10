# bazel-tui-agent

A modern terminal UI for Bazel builds, running inside the Bazel server: a
Java agent injects a module that feeds every build event to a Rust library,
which draws a full-screen dashboard (progress, parallelism and throughput
graphs, running actions, build tree, mnemonics, log) on your terminal. When
output is not a terminal, or on platforms without the native library, Bazel
behaves exactly as without the agent.

<img width="2880" height="1920" alt="image" src="https://github.com/user-attachments/assets/a9f6c598-47ba-4557-b18b-3ad5ede56267" />

## See it run

Recorded from the real terminal stream and regenerated whenever the UI
changes; click any clip for a text player you can pause and copy from.

**`bazel build //...`** — server start, analysis, the dashboard, then the
per-target output listing.

[<img src="https://raw.githubusercontent.com/bluecmd/bazel-tui-agent/media/build.avif" alt="bazel build" width="100%">](https://bluecmd.github.io/bazel-tui-agent/#build)

**`bazel test //...`** — the test overview: counts, failures first, cases
and log paths.

[<img src="https://raw.githubusercontent.com/bluecmd/bazel-tui-agent/media/test.avif" alt="bazel test" width="100%">](https://bluecmd.github.io/bazel-tui-agent/#test)

**A directory output with a remote cache** — the report lists what is
inside, and the cache panel shows where the actions came from.

[<img src="https://raw.githubusercontent.com/bluecmd/bazel-tui-agent/media/cached.avif" alt="bazel build of a directory output" width="100%">](https://bluecmd.github.io/bazel-tui-agent/#cached)

**`bazel help`** — lazy-matching search over every command and option.

[<img src="https://raw.githubusercontent.com/bluecmd/bazel-tui-agent/media/help.avif" alt="bazel help" width="100%">](https://bluecmd.github.io/bazel-tui-agent/#help)

**Failing and flaky tests** on the small workspace shipped with the hook.

[<img src="https://raw.githubusercontent.com/bluecmd/bazel-tui-agent/media/failing.avif" alt="bazel test with failures" width="100%">](https://bluecmd.github.io/bazel-tui-agent/#failing)

## Enable

Download `bazel-tui-agent.jar` from the [releases](../../releases) and add
the startup option to your `~/.bazelrc` (or a workspace `.bazelrc`):

```
startup --host_jvm_args=-javaagent:/absolute/path/to/bazel-tui-agent.jar
```

To try it once from the command line instead:

```sh
bazel --host_jvm_args=-javaagent:/absolute/path/to/bazel-tui-agent.jar build //...
```

Changing the startup option restarts the Bazel server once. Keys while a
build runs: `t` switches the tree order, `l` enlarges the log, Ctrl-C
cancels. `BAZEL_HOOK_UI=0` in the environment turns the UI off.

When the command ends you get a report instead of Bazel's result block: for
`build`, every target's output files with size and whether this run built
them; for `test`, an overview with failures first, failed cases and
`bazel-testlogs` paths (depth follows `--test_summary`). `bazel help` opens
an interactive, fuzzy-matching finder over all commands and options (Enter
prints the entry, Esc prints the usual usage text).

Linux x86_64 only for the UI. One jar runs on Bazel 6.6 through 9.3.

## Build from source

```sh
bazel build //agent:agent_deploy.jar   # -> bazel-bin/agent/agent_deploy.jar
```

## Extra: Bazel autocomplete for fish

If you are using the fish terminal, do not forget the nice tab completion:

```
uv run --with absl-py https://github.com/bazelbuild/bazel/raw/refs/heads/master/scripts/generate_fish_completion.py \
  --bazel=$(which bazel) --output=$HOME/.config/fish/completions/bazel.fish
```
