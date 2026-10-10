package bazelhook;

import java.nio.charset.StandardCharsets;

/**
 * Builds one wire packet for the Rust side (rust/src/wire.rs): little-endian, tag + timestamp +
 * fields in a fixed order per tag. Strings are written as the UTF-8 bytes Bazel keeps
 * internally (one byte per char in its "internal" encoding), length-prefixed.
 */
final class Packet {
  static final byte COMMAND_STARTED = 1;
  static final byte BUILD_STARTED = 2;
  static final byte PHASE = 3;
  static final byte PROGRESS = 4;
  static final byte LOADING_COMPLETE = 5;
  static final byte ANALYSIS_COMPLETE = 6;
  static final byte ACTION_STARTED = 7;
  static final byte ACTION_CACHE_CHECK = 8;
  static final byte ACTION_SCHEDULING = 9;
  static final byte ACTION_RUNNING = 10;
  static final byte ACTION_PROGRESS = 11;
  static final byte ACTION_EXECUTED = 12;
  static final byte ACTION_COMPLETED = 13;
  static final byte SPAWN = 14;
  static final byte TARGET_ANALYZED = 15;
  static final byte TARGET_COMPLETE = 16;
  static final byte TEST_ANALYZED = 17;
  static final byte TEST_FILTERING_COMPLETE = 18;
  static final byte TEST_SUMMARY = 19;
  static final byte BUILD_COMPLETE = 20;
  static final byte NO_BUILD = 21;
  static final byte FETCH = 22;
  static final byte MESSAGE = 23;
  static final byte SYSTEM = 24;
  static final byte COMMAND_FINISHED = 25;
  static final byte HOOK_ERROR = 26;
  static final byte OTHER = 27;
  static final byte TERMINAL_SIZE = 28;

  private byte[] buf = new byte[256];
  private int len;

  Packet(byte tag) {
    u8(tag);
    i64(System.currentTimeMillis());
  }

  private void ensure(int n) {
    if (len + n > buf.length) {
      byte[] b = new byte[Math.max(buf.length * 2, len + n)];
      System.arraycopy(buf, 0, b, 0, len);
      buf = b;
    }
  }

  Packet u8(int v) {
    ensure(1);
    buf[len++] = (byte) v;
    return this;
  }

  Packet bool(boolean v) {
    return u8(v ? 1 : 0);
  }

  Packet i64(long v) {
    ensure(8);
    for (int i = 0; i < 8; i++) {
      buf[len++] = (byte) (v >>> (8 * i));
    }
    return this;
  }

  Packet f64(double v) {
    return i64(Double.doubleToLongBits(v));
  }

  Packet str(String s) {
    if (s == null) {
      s = "";
    }
    byte[] b = latin1(s) ? s.getBytes(StandardCharsets.ISO_8859_1) : s.getBytes(StandardCharsets.UTF_8);
    return bytes(b);
  }

  Packet bytes(byte[] b) {
    if (b == null) {
      b = new byte[0];
    }
    ensure(4 + b.length);
    int n = b.length;
    buf[len++] = (byte) n;
    buf[len++] = (byte) (n >>> 8);
    buf[len++] = (byte) (n >>> 16);
    buf[len++] = (byte) (n >>> 24);
    System.arraycopy(b, 0, buf, len, n);
    len += n;
    return this;
  }

  Packet list(Iterable<?> items) {
    int countPos = len;
    ensure(4);
    len += 4;
    int n = 0;
    for (Object o : items) {
      str(String.valueOf(o));
      n++;
    }
    buf[countPos] = (byte) n;
    buf[countPos + 1] = (byte) (n >>> 8);
    buf[countPos + 2] = (byte) (n >>> 16);
    buf[countPos + 3] = (byte) (n >>> 24);
    return this;
  }

  byte[] finish() {
    byte[] out = new byte[len];
    System.arraycopy(buf, 0, out, 0, len);
    return out;
  }

  /** Bazel's internal strings hold UTF-8 bytes as chars 0..255; anything above is real Unicode. */
  private static boolean latin1(String s) {
    for (int i = 0; i < s.length(); i++) {
      if (s.charAt(i) > 0xFF) {
        return false;
      }
    }
    return true;
  }
}
