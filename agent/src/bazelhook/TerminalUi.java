package bazelhook;

import com.google.devtools.build.lib.runtime.CommandEnvironment;
import com.google.devtools.build.lib.util.io.OutErr;
import java.io.IOException;
import java.io.OutputStream;
import java.util.concurrent.Executors;
import java.util.concurrent.ScheduledExecutorService;
import java.util.concurrent.TimeUnit;

/**
 * Moves bytes between the Rust engine and the client's streams. Every 100 ms, and right after a
 * message, it asks the engine what to write and writes it; nothing is formatted here.
 *
 * <p>The stream is the one Bazel hands its own (silenced) UiEventHandler. The reporter's OutErr
 * would re-post every write as an event and recurse, so that one is never used.
 */
final class TerminalUi {
  private final CommandEnvironment env;
  private final long handle;
  private final ScheduledExecutorService timer;
  private OutErr outErr;
  private boolean closed;

  TerminalUi(CommandEnvironment env, long handle) {
    this.env = env;
    this.handle = handle;
    this.timer =
        Executors.newSingleThreadScheduledExecutor(
            r -> {
              Thread t = new Thread(r, "bazel-hook-ui");
              t.setDaemon(true);
              return t;
            });
    timer.scheduleAtFixedRate(this::flush, 100, 100, TimeUnit.MILLISECONDS);
  }

  private boolean resolveOutErr() {
    if (outErr != null) {
      return true;
    }
    Object handler = Reflect.call(env, "getUiEventHandler"); // Bazel 9+
    if (handler == null) {
      // Older releases: find it in the reporter's handler list.
      Object handlers = Reflect.field(env.getReporter(), "eventHandlers");
      if (handlers == null) {
        handlers = Reflect.field(env.getReporter(), "handlers");
      }
      if (handlers instanceof Iterable) {
        for (Object h : (Iterable<?>) handlers) {
          if (h != null && h.getClass().getSimpleName().equals("UiEventHandler")) {
            handler = h;
          }
        }
      }
    }
    Object stream = handler == null ? null : Reflect.field(handler, "outErr");
    if (stream instanceof OutErr) {
      outErr = (OutErr) stream;
      return true;
    }
    return false;
  }

  synchronized void flush() {
    if (closed || !resolveOutErr()) {
      return;
    }
    try {
      write(Native.poll(handle, System.currentTimeMillis()));
    } catch (Throwable ignored) {
      // never let the UI disturb the build
    }
  }

  /**
   * Blocks while the engine runs an interactive screen (the {@code help} finder), polling it
   * fast enough for keystrokes. Returns at once when nothing interactive is going on.
   */
  void interact() {
    try {
      while (!closed && Native.busy(handle) && !Thread.currentThread().isInterrupted()) {
        flush();
        Thread.sleep(20);
      }
    } catch (InterruptedException e) {
      Thread.currentThread().interrupt();
    } catch (Throwable ignored) {
      // the command result is unaffected
    }
  }

  synchronized void close() {
    if (closed) {
      return;
    }
    closed = true;
    timer.shutdownNow();
    try {
      if (resolveOutErr()) {
        write(Native.finish(handle, System.currentTimeMillis()));
      }
    } catch (Throwable ignored) {
      // nothing to do
    }
  }

  private void write(byte[] framed) throws IOException {
    if (framed == null || framed.length < 8) {
      return;
    }
    int errLen = u32(framed, 0);
    int outLen = u32(framed, 4 + errLen);
    if (errLen > 0) {
      OutputStream s = outErr.getErrorStream();
      s.write(framed, 4, errLen);
      s.flush();
    }
    if (outLen > 0) {
      OutputStream s = outErr.getOutputStream();
      s.write(framed, 8 + errLen, outLen);
      s.flush();
    }
  }

  private static int u32(byte[] b, int at) {
    return (b[at] & 0xff) | (b[at + 1] & 0xff) << 8 | (b[at + 2] & 0xff) << 16 | (b[at + 3] & 0xff) << 24;
  }
}
