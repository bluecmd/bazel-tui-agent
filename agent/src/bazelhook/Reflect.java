package bazelhook;

import java.lang.reflect.Method;
import java.lang.reflect.Modifier;
import java.util.Map;
import java.util.Optional;
import java.util.concurrent.ConcurrentHashMap;

/**
 * Best-effort access to accessors whose names drift between Bazel releases. Lookups are cached
 * per concrete class; a method that does not exist yields {@code null} rather than an error, so
 * features degrade one field at a time instead of taking the whole encoder down.
 */
final class Reflect {
  private static final Map<String, Optional<Method>> CACHE = new ConcurrentHashMap<>();

  private Reflect() {}

  /** Invokes the first zero-argument public method among {@code names} that exists on target. */
  static Object call(Object target, String... names) {
    if (target == null) {
      return null;
    }
    Class<?> cls = target.getClass();
    for (String name : names) {
      Method m = find(cls, name);
      if (m != null) {
        try {
          return m.invoke(target);
        } catch (ReflectiveOperationException | RuntimeException e) {
          return null;
        }
      }
    }
    return null;
  }

  /** Reads a (possibly private) field by name, or null. Only used for Bazel's own UI plumbing. */
  static Object field(Object target, String name) {
    if (target == null) {
      return null;
    }
    for (Class<?> c = target.getClass(); c != null; c = c.getSuperclass()) {
      try {
        java.lang.reflect.Field f = c.getDeclaredField(name);
        f.setAccessible(true);
        return f.get(target);
      } catch (NoSuchFieldException e) {
        // try the superclass
      } catch (RuntimeException | IllegalAccessException e) {
        return null;
      }
    }
    return null;
  }

  static String str(Object target, String... names) {
    Object v = call(target, names);
    return v == null ? null : v.toString();
  }

  static long millis(Object v) {
    if (v instanceof Optional) {
      v = ((Optional<?>) v).orElse(null);
    }
    if (v instanceof java.time.Duration) {
      return ((java.time.Duration) v).toMillis();
    }
    if (v instanceof Number) {
      return ((Number) v).longValue();
    }
    return -1;
  }

  private static Method find(Class<?> cls, String name) {
    String key = cls.getName() + "#" + name;
    Optional<Method> cached = CACHE.get(key);
    if (cached != null) {
      return cached.orElse(null);
    }
    Method found = null;
    // Walk the hierarchy for a public declaring class so invoke() needs no setAccessible.
    for (Class<?> c = cls; c != null && found == null; c = c.getSuperclass()) {
      found = publicMethod(c, name);
      if (found == null) {
        for (Class<?> i : c.getInterfaces()) {
          found = publicMethod(i, name);
          if (found != null) {
            break;
          }
        }
      }
    }
    CACHE.put(key, Optional.ofNullable(found));
    return found;
  }

  private static Method publicMethod(Class<?> c, String name) {
    if (!Modifier.isPublic(c.getModifiers())) {
      return null;
    }
    try {
      Method m = c.getMethod(name);
      return Modifier.isPublic(m.getDeclaringClass().getModifiers()) ? m : null;
    } catch (NoSuchMethodException e) {
      return null;
    }
  }
}
