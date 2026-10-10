package bazelhook;

import java.io.IOException;
import java.io.InputStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;

/**
 * JNI bridge to the Rust library (rust/), shipped inside the jar as
 * {@code native/<os>-<arch>/libbazelhook_ui.<ext>}. Without a library for the server's platform
 * {@link #isAvailable()} is false and the module leaves Bazel alone.
 */
final class Native {
  private static final String LOAD_ERROR;

  static {
    String error = null;
    try {
      load();
    } catch (Throwable t) {
      error = t.toString();
    }
    LOAD_ERROR = error;
  }

  private Native() {}

  static boolean isAvailable() {
    return LOAD_ERROR == null;
  }

  static String loadError() {
    return LOAD_ERROR;
  }

  private static void load() throws IOException {
    String os = System.getProperty("os.name", "").toLowerCase();
    String arch = System.getProperty("os.arch", "").toLowerCase();
    arch = arch.equals("amd64") ? "x86_64" : arch.equals("arm64") ? "aarch64" : arch;
    String osName = os.startsWith("linux") ? "linux" : os.startsWith("mac") ? "macos" : os.startsWith("win") ? "windows" : os;
    String ext = osName.equals("macos") ? ".dylib" : osName.equals("windows") ? ".dll" : ".so";
    String resource = "/native/" + osName + "-" + arch + "/libbazelhook_ui" + ext;
    try (InputStream in = Native.class.getResourceAsStream(resource)) {
      if (in == null) {
        throw new IOException("no bazel-hook library for " + osName + "-" + arch + " in the agent jar");
      }
      Path tmp = Files.createTempFile("bazelhook-", ext);
      tmp.toFile().deleteOnExit();
      Files.copy(in, tmp, StandardCopyOption.REPLACE_EXISTING);
      System.load(tmp.toAbsolutePath().toString());
    }
  }

  // ---- the contract implemented in rust/src/lib.rs ----------------------------------------

  /**
   * New engine. {@code ui} draws on a terminal of {@code width} columns; {@code clientPid} lets the
   * engine find and use the client's terminal (0 = unknown); {@code trace} is a file path or "".
   */
  static native long create(boolean ui, int width, boolean color, int rows, long clientPid, String trace);

  /** One wire packet (see Packet). */
  static native boolean event(long handle, byte[] packet);

  /** Bytes to write now: [u32 stderr len][stderr][u32 stdout len][stdout], or empty. */
  static native byte[] poll(long handle, long nowMs);

  /** Final bytes at the end of the command, same framing. */
  static native byte[] finish(long handle, long nowMs);

  /** True while the engine owns the terminal interactively (the help finder) and must be polled. */
  static native boolean busy(long handle);

  static native void destroy(long handle);
}
