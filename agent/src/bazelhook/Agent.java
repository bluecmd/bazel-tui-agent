package bazelhook;

import java.lang.instrument.Instrumentation;
import java.util.ArrayList;
import java.util.List;
import net.bytebuddy.agent.builder.AgentBuilder;
import net.bytebuddy.asm.Advice;
import net.bytebuddy.implementation.bytecode.assign.Assigner;
import net.bytebuddy.matcher.ElementMatchers;

/**
 * Java agent entry point. Loaded into the Bazel server JVM via
 * {@code --host_jvm_args=-javaagent:/path/to/agent_deploy.jar}.
 *
 * <p>The only thing the agent does with bytecode is to intercept {@code BlazeRuntime.main(...)}
 * and append {@link HookModule} to the list of module classes Bazel instantiates. From then on the
 * hook is an ordinary {@code BlazeModule} and uses only the regular module lifecycle API.
 */
public final class Agent {
  private static volatile boolean installed;
  private static volatile long jarMtimeMs;
  private static volatile String jarPath = "";
  private static volatile long clientPid;
  private static volatile Object terminalSizeMonitor;

  private Agent() {}

  /** Path of the jar this agent was loaded from. */
  public static String jarPath() {
    return jarPath;
  }

  /** Pid of the client whose command is running, from the "pid=NNN" description it sends. */
  public static long clientPid() {
    return clientPid;
  }

  /**
   * Bazel 9.3+ passes a TerminalSizeMonitor to exec(); the client reports column changes to it
   * (SIGWINCH), which is the only resize signal available without the client's pty.
   */
  public static Object terminalSizeMonitor() {
    return terminalSizeMonitor;
  }

  /** Called from the inlined advice; must stay public. */
  public static void noteClientDescription(Object[] args) {
    Object monitor = null;
    for (Object a : args) {
      if (a != null && a.getClass().getSimpleName().equals("TerminalSizeMonitor")) {
        monitor = a;
      }
      if (a instanceof String && ((String) a).startsWith("pid=")) {
        String digits = ((String) a).substring(4).split("[^0-9]")[0];
        try {
          clientPid = digits.isEmpty() ? 0 : Long.parseLong(digits);
        } catch (NumberFormatException e) {
          clientPid = 0;
        }
      }
    }
    terminalSizeMonitor = monitor;
  }

  /** The running Bazel's version string, from BlazeVersionInfo if it is available. */
  public static String bazelVersion() {
    try {
      Class<?> c = Class.forName("com.google.devtools.build.lib.analysis.BlazeVersionInfo");
      Object info = c.getMethod("instance").invoke(null);
      return String.valueOf(c.getMethod("getVersion").invoke(info));
    } catch (Throwable t) {
      return "";
    }
  }

  /** Last-modified time of that jar when the server started; lets the UI spot a stale server. */
  public static long jarMtimeMs() {
    return jarMtimeMs;
  }

  public static void premain(String args, Instrumentation inst) {
    if (installed) {
      return;
    }
    installed = true;
    try {
      java.io.File jar =
          new java.io.File(Agent.class.getProtectionDomain().getCodeSource().getLocation().toURI());
      jarPath = jar.getPath();
      jarMtimeMs = jar.lastModified();
    } catch (Exception ignored) {
      // best effort
    }
    new AgentBuilder.Default()
        .disableClassFormatChanges()
        .with(AgentBuilder.Listener.StreamWriting.toSystemError().withErrorsOnly())
        .type(ElementMatchers.named("com.google.devtools.build.lib.runtime.BlazeRuntime"))
        .transform(
            (builder, typeDescription, classLoader, module, protectionDomain) ->
                builder.visit(
                    Advice.to(MainAdvice.class)
                        .on(
                            ElementMatchers.named("main")
                                .and(ElementMatchers.isStatic())
                                .and(ElementMatchers.takesArgument(0, Iterable.class)))))
        // The client's "pid=NNN" description only ever reaches the dispatcher's exec(); note it so
        // the native side can find the client's terminal (/proc/<pid>/fd).
        .type(ElementMatchers.named("com.google.devtools.build.lib.runtime.BlazeCommandDispatcher"))
        .transform(
            (builder, typeDescription, classLoader, module, protectionDomain) ->
                builder.visit(Advice.to(ExecAdvice.class).on(ElementMatchers.named("exec"))))
        .installOn(inst);
  }

  /** Called from the inlined advice; must stay public. */
  public static Iterable<Class<?>> withHookModule(Iterable<Class<?>> modules) {
    List<Class<?>> out = new ArrayList<>();
    for (Class<?> c : modules) {
      if (c == HookModule.class) {
        return modules;
      }
      out.add(c);
    }
    out.add(HookModule.class);
    return out;
  }

  /** Advice inlined at the start of every {@code BlazeCommandDispatcher.exec(...)} overload. */
  public static final class ExecAdvice {
    private ExecAdvice() {}

    @Advice.OnMethodEnter
    @SuppressWarnings("unused")
    public static void enter(@Advice.AllArguments Object[] args) {
      Agent.noteClientDescription(args);
    }
  }

  /** Advice inlined at the start of {@code BlazeRuntime.main(modules, services, args)}. */
  public static final class MainAdvice {
    private MainAdvice() {}

    @Advice.OnMethodEnter
    @SuppressWarnings("unused")
    public static void enter(
        @Advice.Argument(value = 0, readOnly = false, typing = Assigner.Typing.DYNAMIC)
            Iterable<Class<?>> modules) {
      modules = Agent.withHookModule(modules);
    }
  }
}
