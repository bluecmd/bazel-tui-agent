# TUI agent for Bazel

A modern terminal UI for Bazel builds, running inside the Bazel server: a
Java agent injects a module that feeds every build event to a Rust library,
which draws a full-screen dashboard (progress, parallelism and throughput
graphs, running actions, build tree, mnemonics, log) on your terminal. When
output is not a terminal, or on platforms without the native library, Bazel
behaves exactly as without the agent.

## Quick setup

```sh
curl -fsSL https://raw.githubusercontent.com/bluecmd/bazel-tui-agent/main/install.sh | bash
```

The newest release is installed in
`~/.local/share/bazel-tui-agent/` and a `startup` line is appended to
`~/.bazelrc`. The next `bazel build` in a terminal shows the dashboard. 

<img src="https://raw.githubusercontent.com/bluecmd/bazel-tui-agent/media/dashboard.png" alt="the dashboard during a build" width="100%">

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

Installed with the quick install at the top? Then this is already done. By
hand: download `bazel-tui-agent.jar` from the [releases](../../releases) and
add the startup option to your `~/.bazelrc` (or a workspace `.bazelrc`):

```
startup --host_jvm_args=-javaagent:/absolute/path/to/bazel-tui-agent.jar
```

To try it once from the command line instead:

```sh
bazel --host_jvm_args=-javaagent:/absolute/path/to/bazel-tui-agent.jar build //...
```

Changing the startup option restarts the Bazel server once. Keys while a
build runs: `t` switches the tree order, `l` enlarges the log, `o` opens
the build's page on your Build Event Service in the browser (with
`--bes_results_url`; the address is also printed as a link at the end),
Ctrl-C cancels. In the enlarged log, arrows and PgUp/PgDn scroll (Home for the
top, End to follow the tail again), `←`/`→` shift long lines sideways, `/`
types a search with matches highlighted as you type, `n`/`N` step to the
newer/older match and Esc clears it. Each line shows the time it arrived,
greyed on the left; that is on screen only, the replay into scrollback at
the end is Bazel's output as it was. Actions that just finished stay listed under the running ones
for a few seconds, fading out, so a fast build still shows its work.
`BAZEL_HOOK_UI=0` in the environment turns the UI off.

When the command ends you get a report instead of Bazel's result block: for
`build`, every target's output files with size and whether this run built
them; for `test`, an overview with failures first, failed cases and
`bazel-testlogs` paths (depth follows `--test_summary`). `bazel help` opens
an interactive, fuzzy-matching finder over all commands and options (Enter
prints the entry, Esc prints the usual usage text).

Linux x86_64 only for the UI. One jar runs on Bazel 6.6 through 10.0.0rc1.

## Build from source

```sh
bazel build //agent:agent_deploy.jar   # -> bazel-bin/agent/agent_deploy.jar
```

Such a jar reports itself as `tui dev`. Releases are built with
`--stamp --embed_label=<tag>` (the workflow does this on `v*` tags), which
puts the tag in `bazelhook/version` inside the jar.

To see the UI without a Bazel build, `rust/` also builds a replay tool that
runs the engine on a synthetic build (or on a `BAZEL_HOOK_TRACE` file) in the
terminal it is started in, with that terminal's real size and keys:

```sh
bazel run //rust:bazelhook-replay -- --scenario huge --speed 2
bazel run //rust:bazelhook-replay -- --list     # the scenarios
bazel run //rust:bazelhook-replay -- --probe    # colour bands in the SGR sequences the UI uses
```

`bazel test //rust:all` runs the crate's tests. Releases carry the tool as
`bazelhook-replay-linux-x86_64` next to the jar.

## Extra: Bazel autocomplete for fish

If you are using the fish terminal, do not forget the nice tab completion:

```
uv run --with absl-py https://github.com/bazelbuild/bazel/raw/refs/heads/master/scripts/generate_fish_completion.py \
  --bazel=$(which bazel) --output=$HOME/.config/fish/completions/bazel.fish
```

## Appendix: what the agent consumes from Bazel

For Bazel developers: everything the hook touches inside the server, so
that a change to one of these shows up here. Nothing is modified except
where "advice" is stated; all of it is `com.google.devtools.build.lib`
unless noted. Compiled against the release in `.bazelversion`, run on
6.6 through 9.3; names that moved between releases are tried in order.

### Bytecode advice (Byte Buddy, inlined at method entry)

| Class and method | Why |
| --- | --- |
| `runtime.BlazeRuntime.main(Iterable<Class<? extends BlazeModule>>, …)` | appends `bazelhook.HookModule` to the module list; the only way to load a module |
| `runtime.BlazeCommandDispatcher.exec(…)` (every overload) | reads the client description `pid=NNN` to find the client's pty, and the `TerminalSizeMonitor` argument (9.3+) |
| `server.GrpcServerImpl.cancel(CancelRequest, StreamObserver)` | learns about Ctrl-C the moment the client sends it |
| `runtime.BlazeOptionHandler.parseOptions*(List<String> args, …)` | appends `--show_result=0 --test_summary=none` to the re-parse the dispatcher performs after `beforeCommand`, which discards options parsed by modules |

### Module lifecycle (public API)

`BlazeModule.beforeCommand(CommandEnvironment)` and `afterCommand()`;
`CommandEnvironment.getOptions/getReporter/getEventBus/getCommandName/
getCommandId/getWorkspace/getWorkingDirectory/getExecRoot/getRuntime/
getClientEnv`; `OptionsParser.parse(SOFTWARE_REQUIREMENT, …)` for
`--noshow_progress --curses=no --ui_event_filters=`;
`OptionsParsingResult.getOptions(UiOptions.class)` (`isATty`,
`showProgress`, `useCursesEnum`, `useColor()`, `terminalColumns`),
`BuildRequestOptions.maxResultTargets`, `ExecutionOptions.testSummary`.

### Event bus postings (Guava `EventBus`, `@Subscribe Object`)

| Event class | Accessors used |
| --- | --- |
| `actions.ActionStartedEvent` | `getAction()`, `getNanoTimeStart()` |
| `actions.SchedulingActionEvent`, `RunningActionEvent` | `getActionMetadata()`, `getStrategy()` |
| `actions.CachingActionEvent` (record) | `action()`/`getActionMetadata()`, `strategy()` |
| `actions.ActionProgressEvent` (record) | `action()`, `progressId()`, `progress()`, `finished()` |
| `actions.ActionExecutedEvent` | `getAction()`, `getException()` |
| `actions.ActionCompletionEvent` | `getAction()`, `getRelativeActionStartTimeNanos()`/`getRelativeActionStartTime()`, `getFinishTimeNanos()` |
| `actions.CachedActionEvent` | `getAction()` |
| `actions.SpawnExecutedEvent` | `getSpawn().getMnemonic()`, `getActionMetadata().getPrimaryOutput()`, `getExitCode()`, `getSpawnResult()` → `getRunnerName()`, `getRunnerSubtype()`, `getExecutorHostName()`, `status()`, `isCacheHit()`, `wasRemote()`, `getWallTimeInMs()`/`getWallTime()` |
| `actions.ActionResultReceivedEvent` | `getAction()`, `getActionResult().spawnResults()` (same `SpawnResult` accessors) |
| on every action above, `ActionExecutionMetadata` | `getPrimaryOutput().getExecPathString()`, `getMnemonic()`, `getProgressMessage()`/`prettyPrint()`, `getOwner()` → `getLabel()`, `getTargetKind()`, `getBuildConfigurationMnemonic()`/`getConfigurationMnemonic()`/`getMnemonic()` |
| `analysis.TargetCompleteEvent` | `getLabel()`, `failed()`, `getLegacyFilteredImportantArtifacts()` → `Artifact.getPath()` |
| `buildtool.buildevent.BuildStartingEvent` | `request()`/`getRequest()` → `getTargets()`, `getId()` |
| `skyframe.LoadingPhaseStartedEvent` | `getPackageProgressReceiver()` |
| `pkgcache.LoadingPhaseCompleteEvent` | `getLabels()` |
| `analysis.AnalysisPhaseStartedEvent` | marker only |
| `skyframe.ConfigurationPhaseStartedEvent` | `getAnalysisProgressReceiver()`/`getConfiguredTargetProgressReceiver()` |
| `analysis.AnalysisPhaseCompleteEvent` | `getTopLevelTargets()`, `getTargetsConfigured().total()`, `getActionsConstructed().total()`, `getTimeInMs()` |
| `buildtool.buildevent.ExecutionPhaseStartedEvent`, `skyframe.TopLevelStatusEvents$SomeExecutionStartedEvent` | markers only |
| `buildtool.buildevent.ExecutionProgressReceiverAvailableEvent` | `getExecutionProgressReceiver()` |
| `buildtool.buildevent.MainRepoMappingComputationStartingEvent` | marker only |
| `buildtool.buildevent.TestFilteringCompleteEvent` | `getTargets()`, `getTestTargets()`, `getSkippedTests()` |
| `skyframe.TopLevelStatusEvents$TopLevelTargetAnalyzedEvent`, `$TestAnalyzedEvent` | `configuredTarget()`/`getConfiguredTarget()` → `getLabel()` |
| `runtime.TestSummary` | `getLabel()`, `getStatus()`, `totalRuns()`, `isCached()`, `isSkipped()`, `getFailedTestCases()` (→ `getClassName()`, `getName()`), `getTotalTestCases()`, `getTestTimes()`, `getFailedLogs()`, `getPassedLogs()`, `getNumAttempts()`, `wasUnreportedWrongSize()` |
| `buildtool.buildevent.BuildCompleteEvent` | `getResult()` → `getSuccess()`, `wasCatastrophe()`, `getDetailedExitCode().getExitCode().getNumericExitCode()`, `getStopTime()` |
| `analysis.NoBuildEvent` | marker only |
| `events.ExtendedEventHandler$FetchProgress` | `getResourceIdentifier()`, `getProgress()`, `isFinished()`; on `DownloadProgressEvent` also `getBytesRead()` and the private `totalBytes` field (see below) |

### Reporter

Registered as an `EventHandler` on the command's `Reporter`: every
`events.Event` (`getKind()`, `getMessage()`, `getLocation()`, `getTag()`,
and the attached action output via `getProcessOutput().getStdOut()/
getStdErr()` or `getStdOut()/getStdErr()`), including the STDOUT/STDERR
writes the reporter's `OutErr` re-posts.

### Polled every 100 ms (no events exist for these)

| Object | Call |
| --- | --- |
| `PackageProgressReceiver` (from `LoadingPhaseStartedEvent`) | `progressState()` → `Pair.getFirst()/getSecond()` |
| `AnalysisProgressReceiver` / `ConfiguredTargetProgressReceiver` | `getProgressString()` |
| `ExecutionProgressReceiver` | `getProgressString()` |
| the command thread | `isInterrupted()` (fallback for the cancel advice) |
| JVM | `Runtime` heap, `OperatingSystemMXBean` load and CPU count |

### Private state read by reflection

| What | Why |
| --- | --- |
| `CommandEnvironment.getUiEventHandler()` (9+), else the `Reporter.eventHandlers` queue, then `UiEventHandler.outErr` | the real client stream; the reporter's own `OutErr` would re-post every write as an event |
| `TerminalSizeMonitor` (9.3+): nested `Listener` interface, `addListener`, `terminalSizeChanged(cols, rows)` | width fallback when the client's pty cannot be found |
| `DownloadProgressEvent.totalBytes` (`OptionalLong`, the Content-Length) | the download progress bar; `getBytesRead()` is public but the total has no accessor |

### The `help` index (reflection, `bazel help` only)

`BlazeRuntime.getCommandMap()`, `getBlazeModules()`,
`getRuleClassProvider()`, `getProductName()`; each command's `@Command`
annotation (`name`, `shortDescription`, `help`, `hidden`);
`BlazeCommandUtils.getOptions(commandClass, modules, ruleClassProvider)`
and `getStartupOptions(modules)`;
`OptionsParser.getOptionDefinitions(optionsClass)`; on each
`OptionDefinition`: `getOptionName`, `getAbbreviation`,
`getValueTypeHelpText`, `getConverter().getTypeDescription()`,
`getUnparsedDefaultValue`, `isSpecialNullDefault`,
`getDocumentationCategory`, `getHelpText`, `getOptionEffectTags`,
`getOptionMetadataTags` (HIDDEN and INTERNAL are skipped),
`getOptionExpansion`, `getDeprecationWarning`, `getOldOptionName`,
`allowsMultiple`, `usesBooleanValueSyntax`;
`util.ResourceFileLoader.loadResource(class, name)` for `resource:` help
texts and `target-syntax.txt`.

### Outside the server

The client's pty via `/proc/<client pid>/fd/{0,1,2}`: `TIOCGWINSZ` for the
window size, termios (canonical mode and echo off, `ISIG` kept) to read
keys, restored when the command ends. The workspace's convenience
symlinks (`bazel-bin`, …) are read to pretty-print output paths.
