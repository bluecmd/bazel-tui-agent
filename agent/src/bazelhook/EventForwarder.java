package bazelhook;

import com.google.common.eventbus.AllowConcurrentEvents;
import com.google.common.eventbus.Subscribe;
import com.google.devtools.build.lib.actions.ActionCompletionEvent;
import com.google.devtools.build.lib.actions.ActionExecutedEvent;
import com.google.devtools.build.lib.actions.ActionExecutionMetadata;
import com.google.devtools.build.lib.actions.ActionOwner;
import com.google.devtools.build.lib.actions.ActionStartedEvent;
import com.google.devtools.build.lib.actions.Artifact;
import com.google.devtools.build.lib.actions.RunningActionEvent;
import com.google.devtools.build.lib.actions.SchedulingActionEvent;
import com.google.devtools.build.lib.actions.SpawnExecutedEvent;
import com.google.devtools.build.lib.analysis.AnalysisPhaseCompleteEvent;
import com.google.devtools.build.lib.analysis.TargetCompleteEvent;
import com.google.devtools.build.lib.buildtool.buildevent.BuildCompleteEvent;
import com.google.devtools.build.lib.buildtool.buildevent.ExecutionProgressReceiverAvailableEvent;
import com.google.devtools.build.lib.buildtool.buildevent.TestFilteringCompleteEvent;
import com.google.devtools.build.lib.events.Event;
import com.google.devtools.build.lib.events.EventHandler;
import com.google.devtools.build.lib.events.ExtendedEventHandler.FetchProgress;
import com.google.devtools.build.lib.pkgcache.LoadingPhaseCompleteEvent;
import com.google.devtools.build.lib.runtime.CommandEnvironment;
import com.google.devtools.build.lib.runtime.TestSummary;
import com.google.devtools.build.lib.skyframe.ConfigurationPhaseStartedEvent;
import com.google.devtools.build.lib.skyframe.LoadingPhaseStartedEvent;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.Executors;
import java.util.concurrent.ScheduledExecutorService;
import java.util.concurrent.TimeUnit;
import java.util.function.Function;

/**
 * The probe: turns Bazel's in-process events into wire packets for the Rust engine.
 *
 * <p>Registered on the command's {@code EventBus} (progress and phase events) and {@code Reporter}
 * (messages). A poller samples the phase progress receivers, where Bazel's "[123 / 456]" counters
 * live, and the JVM heap/load.
 *
 * <p>Version resilience: encoders are keyed by event class name and resolved lazily, so an event
 * class absent from the running Bazel never matches, and an encoder that throws a linkage error
 * disables itself after one {@code HookError}. Accessors known to drift go through {@link Reflect}.
 */
final class EventForwarder implements EventHandler {
  private final long handle;
  private final TerminalUi ui;
  private final boolean allEvents;
  private final ScheduledExecutorService poller =
      Executors.newSingleThreadScheduledExecutor(
          r -> {
            Thread t = new Thread(r, "bazel-hook-poll");
            t.setDaemon(true);
            return t;
          });
  private final Map<String, Function<Object, Packet>> encoders = new LinkedHashMap<>();
  private final Map<Class<?>, String> resolved = new ConcurrentHashMap<>();

  private volatile Object packageProgress;
  private volatile Object analysisProgress;
  private volatile Object executionProgress;
  private volatile String lastProgress = "";
  private int pollTick;

  EventForwarder(long handle, TerminalUi ui, boolean allEvents) {
    this.handle = handle;
    this.ui = ui;
    this.allEvents = allEvents;
    registerEncoders();
  }

  private void send(Packet p) {
    Native.event(handle, p.finish());
  }

  void start(CommandEnvironment env) {
    send(
        new Packet(Packet.COMMAND_STARTED)
            .str(env.getCommandName())
            .str(env.getCommandId().toString())
            .str(env.getWorkspace().getPathString())
            .str(env.getWorkingDirectory().getPathString())
            .list(env.getOptions().getResidue())
            .i64(env.getCommandStartTime())
            .str(Agent.bazelVersion())
            .str(Agent.jarPath())
            .i64(Agent.jarMtimeMs()));
    env.getEventBus().register(this);
    env.getReporter().addHandler(this);
    poller.scheduleAtFixedRate(this::pollProgress, 100, 100, TimeUnit.MILLISECONDS);
  }

  void finish() {
    poller.shutdownNow();
    pollProgress();
    send(new Packet(Packet.COMMAND_FINISHED));
  }

  // ---- Reporter: messages ---------------------------------------------------------------------

  @Override
  public void handle(Event event) {
    Packet p =
        new Packet(Packet.MESSAGE)
            .str(event.getKind().name())
            .str(event.getMessage())
            .str(event.getLocation() == null ? "" : event.getLocation().toString())
            .str(event.getTag());
    // Action stdout/stderr attached to the message; the API differs across releases.
    Object po = Reflect.call(event, "getProcessOutput");
    Object out = po != null ? Reflect.call(po, "getStdOut") : Reflect.call(event, "getStdOut");
    Object err = po != null ? Reflect.call(po, "getStdErr") : Reflect.call(event, "getStdErr");
    p.bytes(out instanceof byte[] ? (byte[]) out : null);
    p.bytes(err instanceof byte[] ? (byte[]) err : null);
    send(p);
    if (ui != null) {
      ui.flush();
    }
  }

  // ---- EventBus: everything -------------------------------------------------------------------

  @Subscribe
  @AllowConcurrentEvents
  public void onEvent(Object e) {
    String key = resolve(e.getClass());
    Function<Object, Packet> enc = key == null ? null : encoders.get(key);
    if (enc == null) {
      if (allEvents) {
        send(new Packet(Packet.OTHER).str(e.getClass().getName()));
      }
      return;
    }
    try {
      Packet p = enc.apply(e);
      if (p != null) {
        send(p);
      }
    } catch (Throwable t) {
      synchronized (encoders) {
        encoders.put(key, null);
      }
      send(new Packet(Packet.HOOK_ERROR).str(e.getClass().getName()).str(t.toString()).str(key));
    }
  }

  private String resolve(Class<?> cls) {
    String key = resolved.get(cls);
    if (key != null) {
      return key.isEmpty() ? null : key;
    }
    String found = "";
    List<Class<?>> order = new ArrayList<>();
    for (Class<?> c = cls; c != null && c != Object.class; c = c.getSuperclass()) {
      order.add(c);
    }
    for (Class<?> c = cls; c != null && c != Object.class; c = c.getSuperclass()) {
      for (Class<?> i : c.getInterfaces()) {
        order.add(i);
      }
    }
    for (Class<?> c : order) {
      if (encoders.containsKey(c.getName())) {
        found = c.getName();
        break;
      }
    }
    resolved.put(cls, found);
    return found.isEmpty() ? null : found;
  }

  private void on(String className, Function<Object, Packet> encoder) {
    encoders.put(className, encoder);
  }

  private static final String LIB = "com.google.devtools.build.lib.";

  private static long num(Object o) {
    return o instanceof Number ? ((Number) o).longValue() : 0;
  }

  private void registerEncoders() {
    on(LIB + "actions.ActionStartedEvent", e -> {
      ActionStartedEvent ev = (ActionStartedEvent) e;
      return action(new Packet(Packet.ACTION_STARTED), ev.getAction()).i64(ev.getNanoTimeStart());
    });
    on(LIB + "actions.SchedulingActionEvent", e -> {
      SchedulingActionEvent ev = (SchedulingActionEvent) e;
      return action(new Packet(Packet.ACTION_SCHEDULING), ev.getActionMetadata()).str(ev.getStrategy());
    });
    on(LIB + "actions.RunningActionEvent", e -> {
      RunningActionEvent ev = (RunningActionEvent) e;
      return action(new Packet(Packet.ACTION_RUNNING), ev.getActionMetadata()).str(ev.getStrategy());
    });
    // Records (Java 16) cannot be referenced from Java 11 class files; read them reflectively.
    on(LIB + "actions.CachingActionEvent", e ->
        action(new Packet(Packet.ACTION_CACHE_CHECK), (ActionExecutionMetadata) Reflect.call(e, "action", "getActionMetadata"))
            .str(Reflect.str(e, "strategy", "getStrategy")));
    on(LIB + "actions.ActionProgressEvent", e ->
        action(new Packet(Packet.ACTION_PROGRESS), (ActionExecutionMetadata) Reflect.call(e, "action", "getActionMetadata"))
            .str(Reflect.str(e, "progressId", "getProgressId"))
            .str(Reflect.str(e, "progress", "getProgress"))
            .bool(Boolean.TRUE.equals(Reflect.call(e, "finished", "isFinished"))));
    on(LIB + "actions.ActionExecutedEvent", e -> {
      ActionExecutedEvent ev = (ActionExecutedEvent) e;
      return action(new Packet(Packet.ACTION_EXECUTED), ev.getAction())
          .bool(ev.getException() == null)
          .str(ev.getException() == null ? "" : ev.getException().getMessage());
    });
    on(LIB + "actions.ActionCompletionEvent", e -> {
      ActionCompletionEvent ev = (ActionCompletionEvent) e;
      return action(new Packet(Packet.ACTION_COMPLETED), ev.getAction())
          .i64(num(Reflect.call(ev, "getRelativeActionStartTimeNanos", "getRelativeActionStartTime")))
          .i64(num(Reflect.call(ev, "getFinishTimeNanos")));
    });
    on(LIB + "actions.SpawnExecutedEvent", e -> {
      SpawnExecutedEvent ev = (SpawnExecutedEvent) e;
      Object r = ev.getSpawnResult();
      Artifact primary = ev.getActionMetadata().getPrimaryOutput();
      return new Packet(Packet.SPAWN)
          .str(primary != null ? primary.getExecPathString() : ev.getSpawn().getMnemonic())
          .str(ev.getSpawn().getMnemonic())
          .str(Reflect.str(r, "getRunnerName"))
          .str(Reflect.str(r, "getRunnerSubtype"))
          .str(Reflect.str(r, "getExecutorHostName"))
          .str(Reflect.str(r, "status"))
          .i64(ev.getExitCode())
          .bool(Boolean.TRUE.equals(Reflect.call(r, "isCacheHit")))
          .bool(Boolean.TRUE.equals(Reflect.call(r, "wasRemote")))
          .i64(Reflect.millis(Reflect.call(r, "getWallTimeInMs", "getWallTime")));
    });
    on(LIB + "analysis.TargetCompleteEvent", e -> {
      TargetCompleteEvent ev = (TargetCompleteEvent) e;
      return new Packet(Packet.TARGET_COMPLETE).str(ev.getLabel().toString()).bool(ev.failed());
    });
    on(LIB + "buildtool.buildevent.BuildStartingEvent", e -> {
      Object request = Reflect.call(e, "request", "getRequest");
      Object targets = Reflect.call(request, "getTargets");
      return new Packet(Packet.BUILD_STARTED)
          .list(targets instanceof Iterable ? (Iterable<?>) targets : List.of())
          .str(Reflect.str(request, "getId"));
    });
    on(LIB + "skyframe.LoadingPhaseStartedEvent", e -> {
      packageProgress = ((LoadingPhaseStartedEvent) e).getPackageProgressReceiver();
      return new Packet(Packet.PHASE).u8(0);
    });
    on(LIB + "skyframe.ConfigurationPhaseStartedEvent", e -> {
      analysisProgress = Reflect.call(e, "getAnalysisProgressReceiver", "getConfiguredTargetProgressReceiver");
      return new Packet(Packet.PHASE).u8(1);
    });
    on(LIB + "pkgcache.LoadingPhaseCompleteEvent", e -> {
      packageProgress = null;
      return new Packet(Packet.LOADING_COMPLETE).i64(((LoadingPhaseCompleteEvent) e).getLabels().size());
    });
    on(LIB + "analysis.AnalysisPhaseCompleteEvent", e -> {
      AnalysisPhaseCompleteEvent ev = (AnalysisPhaseCompleteEvent) e;
      analysisProgress = null;
      return new Packet(Packet.ANALYSIS_COMPLETE)
          .i64(ev.getTopLevelTargets().size())
          .i64(num(Reflect.call(Reflect.call(ev, "getTargetsConfigured"), "total")))
          .i64(num(Reflect.call(Reflect.call(ev, "getActionsConstructed"), "total")))
          .i64(num(Reflect.call(ev, "getTimeInMs")));
    });
    on(LIB + "skyframe.TopLevelStatusEvents$SomeExecutionStartedEvent", e -> new Packet(Packet.PHASE).u8(2));
    on(LIB + "buildtool.buildevent.ExecutionPhaseStartedEvent", e -> new Packet(Packet.PHASE).u8(2));
    on(LIB + "analysis.AnalysisPhaseStartedEvent", e -> new Packet(Packet.PHASE).u8(1));
    on(LIB + "buildtool.buildevent.ExecutionProgressReceiverAvailableEvent", e -> {
      executionProgress = ((ExecutionProgressReceiverAvailableEvent) e).getExecutionProgressReceiver();
      return null;
    });
    on(LIB + "buildtool.buildevent.TestFilteringCompleteEvent", e -> {
      TestFilteringCompleteEvent ev = (TestFilteringCompleteEvent) e;
      // Bazel 6 posts this with null collections on a plain `build`.
      return new Packet(Packet.TEST_FILTERING_COMPLETE)
          .i64(ev.getTargets() == null ? 0 : ev.getTargets().size())
          .i64(ev.getTestTargets() == null ? 0 : ev.getTestTargets().size())
          .i64(ev.getSkippedTests() == null ? 0 : ev.getSkippedTests().size());
    });
    on(LIB + "skyframe.TopLevelStatusEvents$TestAnalyzedEvent", e ->
        new Packet(Packet.TEST_ANALYZED).str(Reflect.str(Reflect.call(e, "configuredTarget", "getConfiguredTarget"), "getLabel")));
    on(LIB + "skyframe.TopLevelStatusEvents$TopLevelTargetAnalyzedEvent", e ->
        new Packet(Packet.TARGET_ANALYZED).str(Reflect.str(Reflect.call(e, "configuredTarget", "getConfiguredTarget"), "getLabel")));
    on(LIB + "runtime.TestSummary", e -> {
      TestSummary ev = (TestSummary) e;
      long totalMs = 0;
      for (Long t : ev.getTestTimes()) {
        totalMs += t;
      }
      return new Packet(Packet.TEST_SUMMARY)
          .str(ev.getLabel().toString())
          .str(ev.getStatus().name())
          .i64(ev.totalRuns())
          .bool(ev.isCached())
          .bool(ev.isSkipped())
          .i64(ev.getFailedTestCases().size())
          .i64(ev.getTotalTestCases())
          .i64(totalMs);
    });
    on(LIB + "buildtool.buildevent.BuildCompleteEvent", e -> {
      BuildCompleteEvent ev = (BuildCompleteEvent) e;
      return new Packet(Packet.BUILD_COMPLETE)
          .bool(ev.getResult().getSuccess())
          .bool(ev.getResult().wasCatastrophe())
          .i64(num(Reflect.call(Reflect.call(Reflect.call(ev.getResult(), "getDetailedExitCode"), "getExitCode"), "getNumericExitCode")))
          .i64(ev.getResult().getStopTime());
    });
    on(LIB + "analysis.NoBuildEvent", e -> new Packet(Packet.NO_BUILD));
    // The silent stretch before loading (module resolution) has one marker event; surface it as
    // the same kind of activity message Bazel's own UI shows.
    on(LIB + "buildtool.buildevent.MainRepoMappingComputationStartingEvent", e ->
        new Packet(Packet.MESSAGE).str("PROGRESS").str("Computing main repo mapping").str("").str("").bytes(null).bytes(null));
    on(LIB + "events.ExtendedEventHandler$FetchProgress", e -> {
      FetchProgress ev = (FetchProgress) e;
      return new Packet(Packet.FETCH).str(ev.getResourceIdentifier()).str(ev.getProgress()).bool(ev.isFinished());
    });
  }

  private static Packet action(Packet p, ActionExecutionMetadata a) {
    Artifact primary = a.getPrimaryOutput();
    ActionOwner owner = a.getOwner();
    String description;
    try {
      description = a.getProgressMessage();
    } catch (RuntimeException ignored) {
      description = a.prettyPrint();
    }
    return p.str(primary != null ? primary.getExecPathString() : a.prettyPrint())
        .str(a.getMnemonic())
        .str(owner == null || owner.getLabel() == null ? "" : owner.getLabel().toString())
        .str(owner == null ? "" : owner.getTargetKind())
        .str(owner == null ? "" : Reflect.str(owner, "getBuildConfigurationMnemonic", "getConfigurationMnemonic", "getMnemonic"))
        .str(description);
  }

  // ---- Progress poller ------------------------------------------------------------------------

  /** Samples every live phase counter (Bazel 9 overlaps analysis and execution). */
  private void pollProgress() {
    try {
      if (pollTick++ % 10 == 0) {
        Runtime rt = Runtime.getRuntime();
        java.lang.management.OperatingSystemMXBean os = java.lang.management.ManagementFactory.getOperatingSystemMXBean();
        send(new Packet(Packet.SYSTEM)
            .i64((rt.totalMemory() - rt.freeMemory()) >> 20)
            .i64(rt.maxMemory() >> 20)
            .f64(os.getSystemLoadAverage())
            .i64(os.getAvailableProcessors()));
      }
      String loading = "";
      String detail = "";
      Object pkg = packageProgress;
      if (pkg != null) {
        Object state = Reflect.call(pkg, "progressState");
        loading = nz(Reflect.str(state, "getFirst", "first"));
        detail = nz(Reflect.str(state, "getSecond", "second"));
      }
      String analysis = nz(Reflect.str(analysisProgress, "getProgressString"));
      String execution = nz(Reflect.str(executionProgress, "getProgressString"));
      if (loading.isEmpty() && analysis.isEmpty() && execution.isEmpty()) {
        return;
      }
      String key = loading + "|" + detail + "|" + analysis + "|" + execution;
      if (!key.equals(lastProgress)) {
        lastProgress = key;
        send(new Packet(Packet.PROGRESS).str(loading).str(detail).str(analysis).str(execution));
      }
    } catch (Throwable t) {
      send(new Packet(Packet.HOOK_ERROR).str("poll").str(t.toString()).str(""));
    }
  }

  private static String nz(String s) {
    return s == null ? "" : s;
  }
}
