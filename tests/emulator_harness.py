"""Shared real-binary selection and bounded interactive subprocess I/O."""
from collections import deque
import os
from pathlib import Path
import queue
import subprocess
import sys
import threading

WORKSPACE = Path(__file__).resolve().parents[1]
DEFAULT_TIMEOUT = float(os.environ.get("EMULATOR_TEST_TIMEOUT", "10"))


def resolve_binary(environ=None, workspace=WORKSPACE):
    env = os.environ if environ is None else environ
    backend = env.get("EMULATOR_TEST_BACKEND", "native")
    if backend not in ("native", "mock"):
        raise RuntimeError("EMULATOR_TEST_BACKEND must be native or mock")
    if backend == "mock":
        return str(workspace / "tests" / "mock_emulator.py")
    explicit = env.get("EMULATOR_BIN")
    suffix = ".exe" if sys.platform == "win32" else ""
    candidates = [Path(explicit).expanduser()] if explicit else [
        workspace / "build" / "bin" / "Release" / ("clothing_app" + suffix),
        workspace / "build" / "bin" / ("clothing_app" + suffix),
    ]
    for candidate in candidates:
        if candidate.is_file() and candidate.suffix.lower() != ".py":
            return str(candidate.resolve())
    raise RuntimeError("Native emulator not found. Build clothing_app and set EMULATOR_BIN "
                       "to its absolute path; use EMULATOR_TEST_BACKEND=mock only for mock tests.")


class _Input:
    def __init__(self, owner, stream):
        self.owner = owner
        self.stream = stream
        self.tasks = queue.Queue()
        self.thread = threading.Thread(target=self._run, daemon=True)
        self.thread.start()

    def _run(self):
        while True:
            operation, value, result = self.tasks.get()
            if operation is None:
                return
            try:
                result.put((True, getattr(self.stream, operation)(value) if operation == "write"
                            else self.stream.flush()))
            except Exception as error:
                result.put((False, error))
                return

    def _call(self, operation, value=None):
        result = queue.Queue(maxsize=1)
        self.tasks.put((operation, value, result))
        try:
            ok, value = result.get(timeout=self.owner.timeout)
        except queue.Empty:
            self.owner._timeout("stdin " + operation)
        if not ok:
            raise value
        return value

    def write(self, value):
        return self._call("write", value)

    def flush(self):
        return self._call("flush")


class _Output:
    def __init__(self, owner, stream):
        self.owner = owner
        self.lines = queue.Queue()
        self.eof = False
        self.thread = threading.Thread(target=self._read, args=(stream,), daemon=True)
        self.thread.start()

    def _read(self, stream):
        try:
            for line in stream:
                self.lines.put(line)
        finally:
            self.lines.put(None)

    def readline(self):
        if self.eof:
            return ""
        try:
            line = self.lines.get(timeout=self.owner.timeout)
        except queue.Empty:
            self.owner._timeout("stdout response")
        if line is None:
            self.eof = True
            return ""
        return line


class TimedProcess:
    """Popen-compatible pipes used by the existing interactive assertions."""
    def __init__(self, *args, timeout=DEFAULT_TIMEOUT, **kwargs):
        self.timeout = timeout
        self._closed = False
        self._process = subprocess.Popen(*args, **kwargs)
        self.stderr_lines = deque(maxlen=80)
        self.stdout = _Output(self, self._process.stdout)
        self.stdin = _Input(self, self._process.stdin)
        self.stderr_thread = threading.Thread(target=self._drain_stderr, daemon=True)
        self.stderr_thread.start()

    def _drain_stderr(self):
        for line in self._process.stderr:
            self.stderr_lines.append(line)

    def _timeout(self, operation):
        self.terminate()
        self.wait()
        raise TimeoutError(f"Emulator exceeded {self.timeout}s waiting for {operation}. "
                           f"stderr: {''.join(self.stderr_lines)}")

    def poll(self):
        return self._process.poll()

    def terminate(self):
        if self.poll() is None:
            self._process.terminate()

    def wait(self, timeout=None):
        try:
            code = self._process.wait(timeout=self.timeout if timeout is None else timeout)
        except subprocess.TimeoutExpired:
            self._process.kill()
            code = self._process.wait(timeout=self.timeout)
        if not self._closed:
            self._closed = True
            self.stdin.tasks.put((None, None, None))
            self.stdin.thread.join(timeout=self.timeout)
            self.stdout.thread.join(timeout=self.timeout)
            self.stderr_thread.join(timeout=self.timeout)
            for stream in (self._process.stdin, self._process.stdout, self._process.stderr):
                stream.close()
        return code

    def close(self):
        self.terminate()
        self.wait()


def spawn_interactive(binary, temp_dir, extra_args=()):
    command = [sys.executable, binary] if binary.endswith(".py") else [binary]
    command += ["--headless", "--test-mode", "--interactive", *extra_args]
    env = os.environ.copy()
    env["ALLOWED_DUMP_DIR"] = str(temp_dir)
    process = TimedProcess(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                           stderr=subprocess.PIPE, text=True, env=env, cwd=WORKSPACE)
    try:
        ready = process.stdout.readline().strip()
        if ready != "MOCK_EMULATOR_READY":
            raise RuntimeError(f"Emulator did not become ready: {ready!r}")
    except Exception:
        process.close()
        raise
    return process
