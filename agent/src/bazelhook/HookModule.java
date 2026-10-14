package bazelhook;

import com.google.common.collect.ImmutableList;
import com.google.devtools.build.lib.events.Event;
import com.google.devtools.build.lib.runtime.BlazeModule;
import com.google.devtools.build.lib.runtime.CommandEnvironment;
import com.google.devtools.build.lib.runtime.UiOptions;
import com.google.devtools.common.options.OptionPriority.PriorityCategory;
import com.google.devtools.common.options.OptionsParser;
import java.util.Map;

/**
 * The module injected by {@link Agent}. Per command it decides whether the Rust engine runs:
 *
 * <ul>
 *   <li>the terminal UI, under the same conditions as Bazel's own curses bar (client on a TTY,
 *       {@code --show_progress}, {@code --curses} not {@code no}); {@code BAZEL_HOOK_UI=0} in the
 *       client environment disables it, {@code BAZEL_HOOK_ROWS} sets the block height;
 *   <li>an event trace to the file named by {@code BAZEL_HOOK_TRACE} (test and debugging aid).
 * </ul>
 *
 * Without the native library for this platform, or with neither feature wanted, nothing happens
 * and Bazel's output is exactly as without the agent. The server JVM keeps running between
 * commands, so one module instance sees many commands, strictly one at a time.
 */
public final class HookModule extends BlazeModule {
  private EventForwarder forwarder;
  private TerminalUi ui;
  private long handle;

  @Override
  public void beforeCommand(CommandEnvironment env) {
    try {
      Map<String, String> clientEnv = env.getClientEnv();
      String trace = clientEnv.getOrDefault("BAZEL_HOOK_TRACE", "");
      boolean wantUi = wantsTerminalUi(env, clientEnv);
      if (!wantUi && trace.isEmpty()) {
        return;
      }
      if (!Native.isAvailable()) {
        if ("1".equals(clientEnv.get("BAZEL_HOOK_DEBUG"))) {
          env.getReporter().handle(Event.info("bazel-hook: inactive: " + Native.loadError()));
        }
        return;
      }
      UiOptions opts = env.getOptions().getOptions(UiOptions.class);
      int width = opts != null && opts.terminalColumns > 0 ? opts.terminalColumns : 80;
      int rows = 12;
      try {
        rows = Integer.parseInt(clientEnv.getOrDefault("BAZEL_HOOK_ROWS", "12"));
      } catch (NumberFormatException ignored) {
        // keep default
      }
      handle = Native.create(wantUi, width, opts != null && opts.useColor(), truecolor(clientEnv), rows, Agent.clientPid(), trace);
      if (wantUi) {
        // Silence UiEventHandler completely; the engine prints the messages itself.
        parse(env, "--noshow_progress", "--curses=no", "--ui_event_filters=");
        ui = new TerminalUi(env, handle);
      }
      forwarder = new EventForwarder(handle, ui, "1".equals(clientEnv.get("BAZEL_HOOK_ALL_EVENTS")));
      forwarder.start(env);
      listenForResizes(handle);
      if (wantUi) {
        // The engine prints its own build result and test overview (the values Bazel would have
        // used went out with COMMAND_STARTED, so the engine can honour them).
        java.util.List<String> extra = new java.util.ArrayList<>();
        if (EventForwarder.options(env, "com.google.devtools.build.lib.buildtool.BuildRequestOptions") != null) {
          extra.add("--show_result=0");
        }
        if (EventForwarder.options(env, "com.google.devtools.build.lib.exec.ExecutionOptions") != null) {
          extra.add("--test_summary=none");
        }
        if (!extra.isEmpty()) {
          // Both ways: this parser for releases that keep it, the re-parse for those that do not.
          parse(env, extra.toArray(new String[0]));
          Agent.setExtraOptions(extra);
        }
        if ("help".equals(env.getCommandName()) && env.getOptions().getResidue().isEmpty()) {
          HelpIndex.send(env, handle);
        }
      }
    } catch (Throwable t) {
      // The hook must never take the build down with it.
      env.getReporter().handle(Event.warn("bazel-hook: disabled after error: " + t));
      forwarder = null;
      ui = null;
    }
  }

  @Override
  public void afterCommand() {
    Agent.setExtraOptions(null);
    try {
      if (ui != null) {
        ui.interact();
      }
      if (forwarder != null) {
        forwarder.finish();
      }
      if (ui != null) {
        ui.close();
      }
      if (handle != 0) {
        Native.destroy(handle);
      }
    } catch (Throwable ignored) {
      // nothing to do
    } finally {
      forwarder = null;
      ui = null;
      handle = 0;
    }
  }

  /**
   * Bazel 9.3+ forwards the client's terminal width on SIGWINCH to a TerminalSizeMonitor that is
   * handed to the dispatcher. Subscribe through a reflective proxy so the agent has no compile-time
   * dependency on that class (older servers simply have no monitor).
   */
  private static void listenForResizes(long handle) {
    Object monitor = Agent.terminalSizeMonitor();
    if (monitor == null) {
      return;
    }
    try {
      Class<?> listener = null;
      for (Class<?> c : monitor.getClass().getDeclaredClasses()) {
        if (c.isInterface() && c.getSimpleName().equals("Listener")) {
          listener = c;
        }
      }
      if (listener == null) {
        return;
      }
      Object proxy =
          java.lang.reflect.Proxy.newProxyInstance(
              listener.getClassLoader(),
              new Class<?>[] {listener},
              (p, method, args) -> {
                if (method.getName().equals("terminalSizeChanged") && args != null && args.length == 2) {
                  Native.event(handle, new Packet(Packet.TERMINAL_SIZE).i64(((Number) args[0]).longValue()).i64(((Number) args[1]).longValue()).finish());
                  return null;
                }
                if (method.getName().equals("hashCode")) {
                  return System.identityHashCode(p);
                }
                if (method.getName().equals("equals")) {
                  return p == args[0];
                }
                return method.getName().equals("toString") ? "bazel-hook size listener" : null;
              });
      monitor.getClass().getMethod("addListener", listener).invoke(monitor, proxy);
    } catch (ReflectiveOperationException | RuntimeException ignored) {
      // no resize notifications on this Bazel; the inline block keeps its initial width
    }
  }

  /** Same conditions under which Bazel itself would draw its curses progress bar. */
  /** 24-bit colour: what COLORTERM in the client's environment says, BAZEL_HOOK_TRUECOLOR=0/1 overriding. */
  private static boolean truecolor(Map<String, String> clientEnv) {
    String force = clientEnv.get("BAZEL_HOOK_TRUECOLOR");
    if (force != null) {
      return "1".equals(force);
    }
    String ct = clientEnv.getOrDefault("COLORTERM", "");
    return ct.equals("truecolor") || ct.equals("24bit");
  }

  private static boolean wantsTerminalUi(CommandEnvironment env, Map<String, String> clientEnv) {
    if ("0".equals(clientEnv.get("BAZEL_HOOK_UI"))) {
      return false;
    }
    UiOptions opts = env.getOptions().getOptions(UiOptions.class);
    return opts != null && opts.isATty && opts.showProgress && opts.useCursesEnum != UiOptions.UseCurses.NO;
  }

  /**
   * The UI handler is constructed after {@code beforeCommand} and re-reads {@code UiOptions} from
   * the parser at that point, so adding options here is enough to change its behaviour.
   */
  private static void parse(CommandEnvironment env, String... options) {
    if (env.getOptions() instanceof OptionsParser) {
      OptionsParser parser = (OptionsParser) env.getOptions();
      try {
        parser.parse(PriorityCategory.SOFTWARE_REQUIREMENT, "bazel-hook", ImmutableList.copyOf(options));
      } catch (Exception e) {
        env.getReporter().handle(Event.warn("bazel-hook: could not set options: " + e.getMessage()));
      }
    }
  }
}
