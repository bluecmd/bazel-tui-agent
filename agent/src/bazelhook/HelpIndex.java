package bazelhook;

import com.google.devtools.build.lib.runtime.BlazeCommand;
import com.google.devtools.build.lib.runtime.BlazeRuntime;
import com.google.devtools.build.lib.runtime.Command;
import com.google.devtools.build.lib.runtime.CommandEnvironment;
import java.lang.reflect.Method;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Collections;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;
import java.util.TreeSet;

/**
 * Everything {@code bazel help} documents, sent as packets for the engine's interactive finder:
 * one {@code HELP_COMMAND} per visible command (plus the target-syntax topic) and one
 * {@code HELP_OPTION} per documented option, with the commands that accept it.
 *
 * <p>Uses the same sources as Bazel's HelpCommand (the {@code @Command} annotations and the option
 * definitions of each command's option classes), through reflection where the signatures moved
 * between releases. Anything that cannot be read is left out rather than failing the command.
 */
final class HelpIndex {
  private static final String LIB = "com.google.devtools.build.lib.";

  private static final class Opt {
    final Object def;
    final TreeSet<String> commands = new TreeSet<>();

    Opt(Object def) {
      this.def = def;
    }
  }

  private HelpIndex() {}

  static void send(CommandEnvironment env, long handle) {
    try {
      BlazeRuntime rt = env.getRuntime();
      String product = rt.getProductName();
      Map<String, BlazeCommand> commands = new TreeMap<>(rt.getCommandMap());
      Map<String, Opt> options = new TreeMap<>();
      for (Map.Entry<String, BlazeCommand> e : commands.entrySet()) {
        Command ann = e.getValue().getClass().getAnnotation(Command.class);
        if (ann == null || ann.hidden()) {
          continue;
        }
        String help = ann.help();
        if (help.startsWith("resource:")) {
          // Newer releases keep the text next to the command class.
          String text = resource(e.getValue().getClass(), help.substring("resource:".length()));
          help = text == null ? "" : text;
        }
        help = help.replace("%{product}", product).replace("%{command}", e.getKey()).replace("%{options}", "").trim();
        Native.event(
            handle,
            new Packet(Packet.HELP_COMMAND)
                .str(e.getKey())
                .str(ann.shortDescription().replace("%{product}", product))
                .str(help)
                .finish());
        for (Object cls : commandOptionClasses(rt, e.getValue().getClass())) {
          collect(options, cls, e.getKey());
        }
      }
      for (Object cls : startupOptionClasses(rt)) {
        collect(options, cls, "startup");
      }
      String syntax = null;
      try {
        syntax = resource(Class.forName(LIB + "runtime.commands.HelpCommand", false, HelpIndex.class.getClassLoader()), "target-syntax.txt");
      } catch (ClassNotFoundException ignored) {
        // no such topic
      }
      if (syntax != null) {
        Native.event(
            handle,
            new Packet(Packet.HELP_COMMAND)
                .str("target-syntax")
                .str("Explains the syntax for specifying targets.")
                .str(syntax.replace("%{product}", product).trim())
                .finish());
      }
      for (Map.Entry<String, Opt> e : options.entrySet()) {
        byte[] p = option(e.getKey(), e.getValue());
        if (p != null) {
          Native.event(handle, p);
        }
      }
    } catch (Throwable t) {
      Native.event(handle, new Packet(Packet.HOOK_ERROR).str("help-index").str(t.toString()).str("").finish());
    }
  }

  private static byte[] option(String name, Opt o) {
    Object d = o.def;
    List<String> meta = names(Reflect.call(d, "getOptionMetadataTags"));
    if (meta.contains("HIDDEN") || meta.contains("INTERNAL")) {
      return null;
    }
    Object abbrev = Reflect.call(d, "getAbbreviation");
    String ab = abbrev instanceof Character && (Character) abbrev != '\0' ? abbrev.toString() : "";
    String valueType = nz(Reflect.str(d, "getValueTypeHelpText"));
    if (valueType.isEmpty()) {
      Object conv = Reflect.call(d, "getConverter");
      valueType = nz(Reflect.str(conv, "getTypeDescription"));
    }
    String def = Boolean.TRUE.equals(Reflect.call(d, "isSpecialNullDefault")) ? "" : nz(Reflect.str(d, "getUnparsedDefaultValue"));
    Object exp = Reflect.call(d, "getOptionExpansion");
    List<String> expansion = exp instanceof String[] ? Arrays.asList((String[]) exp) : Collections.emptyList();
    return new Packet(Packet.HELP_OPTION)
        .str(name)
        .str(ab)
        .str(valueType)
        .str(def)
        .str(nz(Reflect.str(d, "getDocumentationCategory")))
        .str(nz(Reflect.str(d, "getHelpText")))
        .list(o.commands)
        .list(names(Reflect.call(d, "getOptionEffectTags")))
        .list(meta)
        .list(expansion)
        .str(nz(Reflect.str(d, "getDeprecationWarning")))
        .str(nz(Reflect.str(d, "getOldOptionName")))
        .bool(Boolean.TRUE.equals(Reflect.call(d, "allowsMultiple")))
        .bool(Boolean.TRUE.equals(Reflect.call(d, "usesBooleanValueSyntax")))
        .finish();
  }

  private static void collect(Map<String, Opt> options, Object optionsClass, String command) {
    Object defs = invokeStatic("com.google.devtools.common.options.OptionsParser", "getOptionDefinitions", optionsClass);
    if (!(defs instanceof Iterable)) {
      return;
    }
    for (Object d : (Iterable<?>) defs) {
      String name = Reflect.str(d, "getOptionName");
      if (name == null || name.isEmpty()) {
        continue;
      }
      options.computeIfAbsent(name, k -> new Opt(d)).commands.add(command);
    }
  }

  /** BlazeCommandUtils.getOptions(commandClass, modules, ruleClassProvider), whatever its exact signature. */
  private static Iterable<?> commandOptionClasses(BlazeRuntime rt, Class<?> commandClass) {
    Object r = invokeStatic(LIB + "runtime.BlazeCommandUtils", "getOptions", commandClass, rt.getBlazeModules(), rt.getRuleClassProvider());
    return r instanceof Iterable ? (Iterable<?>) r : Collections.emptyList();
  }

  private static Iterable<?> startupOptionClasses(BlazeRuntime rt) {
    Object r = invokeStatic(LIB + "runtime.BlazeCommandUtils", "getStartupOptions", rt.getBlazeModules());
    return r instanceof Iterable ? (Iterable<?>) r : Collections.emptyList();
  }

  /** A text resource next to a class, the way Bazel's ResourceFileLoader finds it (or null). */
  private static String resource(Class<?> relativeTo, String name) {
    Object r = invokeStatic(LIB + "util.ResourceFileLoader", "loadResource", relativeTo, name);
    return r instanceof String ? (String) r : null;
  }

  private static Object invokeStatic(String className, String method, Object... args) {
    try {
      Class<?> cls = Class.forName(className, false, HelpIndex.class.getClassLoader());
      for (Method m : cls.getMethods()) {
        if (m.getName().equals(method) && m.getParameterCount() == args.length) {
          return m.invoke(null, args);
        }
      }
    } catch (ReflectiveOperationException | RuntimeException | LinkageError e) {
      // not available on this release
    }
    return null;
  }

  private static List<String> names(Object array) {
    List<String> out = new ArrayList<>();
    if (array instanceof Object[]) {
      for (Object o : (Object[]) array) {
        out.add(String.valueOf(o));
      }
    }
    return out;
  }

  private static String nz(String s) {
    return s == null ? "" : s;
  }
}
