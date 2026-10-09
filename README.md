# bazel-tui-agent

A modern terminal UI for Bazel builds, running inside the Bazel server: a
Java agent injects a module that feeds every build event to a Rust library,
which draws a full-screen dashboard (progress, parallelism and throughput
graphs, running actions, build tree, mnemonics, log) on your terminal. When
output is not a terminal, or on platforms without the native library, Bazel
behaves exactly as without the agent.

Demo: https://asciinema.org/a/TkLD0Ppus3PXVGHz

<img width="2880" height="1920" alt="image" src="https://github.com/user-attachments/assets/a9f6c598-47ba-4557-b18b-3ad5ede56267" />

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

Linux x86_64 only for the UI. One jar runs on Bazel 6.6 through 9.3.

## Build from source

```sh
bazel build //agent:agent_deploy.jar   # -> bazel-bin/agent/agent_deploy.jar
```
