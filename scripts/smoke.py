#!/usr/bin/env python3
"""Real-process smoke checks. Python 3.11+, Unix PTY; --pulse requires Linux.

PulseAudio is isolated in a temporary user-owned runtime directory. No host
server, devices, configuration, or sound output are used or changed.
"""

import argparse
import codecs
import errno
import fcntl
import json
import math
import os
from pathlib import Path
import pty
import re
import select
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time


class Screen:
    """Decode the cursor/clear subset emitted by this app, not a full VT emulator."""

    def __init__(self, cols=80, rows=28):
        self.cols, self.rows = cols, rows
        self.cells = [[" "] * cols for _ in range(rows)]
        self.x = self.y = 0
        self.pending = ""
        self.decoder = codecs.getincrementaldecoder("utf-8")("replace")

    def feed(self, data):
        self.pending += self.decoder.decode(data)
        while self.pending:
            if self.pending.startswith("\x1b"):
                match = re.match(r"\x1b\[([0-9;?]*)([@-~])", self.pending)
                if not match:
                    # A read can end in the middle of a CSI sequence.
                    if len(self.pending) == 1 or self.pending.startswith("\x1b["):
                        return
                    raise AssertionError("Unexpected non-CSI terminal escape")
                params, op = match.groups()
                self.pending = self.pending[match.end():]
                if op in ("H", "f"):
                    parts = [int(p or 1) for p in params.split(";")]
                    self.y, self.x = parts[0] - 1, (parts[1] if len(parts) > 1 else 1) - 1
                elif op == "J" and params == "2":
                    self.cells = [[" "] * self.cols for _ in range(self.rows)]
                continue
            char, self.pending = self.pending[0], self.pending[1:]
            if char == "\r":
                self.x = 0
            elif char == "\n":
                self.y += 1
            elif char >= " " and char != "\x7f":
                if 0 <= self.x < self.cols and 0 <= self.y < self.rows:
                    self.cells[self.y][self.x] = char
                self.x += 1

    def text(self):
        return "\n".join("".join(row) for row in self.cells)

    def field(self):
        return tuple("".join(row) for row in self.cells[2:-2])

    def bars(self):
        return sum(char in "▁▂▃▄▅▆▇█" for row in self.field() for char in row)


class Terminal:
    def __init__(self, binary, args, env=None, pcm=False):
        self.raw = bytearray()
        self.screen = Screen()
        self.status = None
        self.pcm = None
        self.eof = False
        gate_read, gate_write = os.pipe()
        pcm_read, pcm_write = os.pipe() if pcm else (None, None)
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            try:
                os.close(gate_write)
                os.read(gate_read, 1)
                os.close(gate_read)
                if pcm:
                    os.close(pcm_write)
                    os.dup2(pcm_read, 0)
                    os.close(pcm_read)
                child_env = dict(os.environ, TERM="xterm-256color", COLORTERM="truecolor")
                child_env.pop("NO_COLOR", None)
                child_env.update(env or {})
                os.execve(binary, [binary, *args], child_env)
            except BaseException:
                os._exit(127)
        os.close(gate_read)
        if pcm:
            os.close(pcm_read)
            self.pcm = pcm_write
            os.set_blocking(self.pcm, False)
        # Gate exec until baseline attributes and dimensions have been recorded.
        self.original = termios.tcgetattr(self.fd)
        self.resize(80, 28)
        os.set_blocking(self.fd, False)
        os.write(gate_write, b"x")
        os.close(gate_write)

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close_pcm()
        if self.status is None:
            try:
                os.kill(self.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.waitpid(self.pid, 0)
        os.close(self.fd)

    def resize(self, cols, rows):
        self.screen = Screen(cols, rows)
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

    def pump(self, duration):
        deadline = time.monotonic() + duration
        while time.monotonic() < deadline:
            if self.eof:
                time.sleep(max(0, deadline - time.monotonic()))
                break
            ready, _, _ = select.select([self.fd], [], [], max(0, deadline - time.monotonic()))
            if not ready:
                break
            try:
                data = os.read(self.fd, 65536)
            except OSError as error:
                if error.errno != errno.EIO:
                    raise
                data = b""
            if not data:
                self.eof = True
                break
            self.raw.extend(data)
            self.screen.feed(data)

    def until(self, predicate, description, timeout=5):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self.pump(0.04)
            if predicate():
                return
            if self.eof:
                break
        raise AssertionError(f"Timed out: {description}\n{self.screen.text()}")

    def expect(self, text):
        self.until(lambda: text in self.screen.text(), f"visible {text!r}")

    def send(self, keys):
        os.write(self.fd, keys)

    def write_pcm(self, data):
        deadline = time.monotonic() + 3
        view = memoryview(data)
        while view:
            if time.monotonic() >= deadline:
                raise AssertionError("PCM producer blocked: capture did not drain")
            try:
                count = os.write(self.pcm, view)
                view = view[count:]
            except BlockingIOError:
                self.pump(0.01)

    def close_pcm(self):
        if self.pcm is not None:
            os.close(self.pcm)
            self.pcm = None

    def finish(self, key=b"q", expected=0, entered=True):
        if key:
            self.send(key)
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            self.pump(0.03)
            pid, status = os.waitpid(self.pid, os.WNOHANG)
            if pid:
                self.status = os.waitstatus_to_exitcode(status)
                break
        assert self.status == expected, f"Exit {self.status}, expected {expected}"
        assert termios.tcgetattr(self.fd) == self.original, "Terminal attributes not restored"
        if entered:
            assert b"\x1b[?1049l" in self.raw, "Alternate screen not restored"
            assert b"\x1b[?25h" in self.raw, "Cursor not restored"


def demo_checks(binary, snapshot):
    with Terminal(binary, ["--demo", "--trails"]) as app:
        app.expect("DEMO")
        app.pump(1.2)
        assert app.screen.bars() > 100, "Demo spectrum did not rise"
        if snapshot:
            Path(snapshot).write_text(app.screen.text() + "\n", encoding="utf-8")
        app.send(b"++++++++++++")
        app.expect("BEATFALL  8")
        app.send(b"------------")
        app.expect("BEATFALL  1")
        app.send(b"p")
        app.expect("PAUSED")
        frozen = app.screen.field()
        app.pump(0.3)
        assert app.screen.field() == frozen, "Animation advanced while paused"
        app.send(b"p")
        app.expect("aurora")
        app.send(b"t")
        app.expect("ember")
        app.send(b"h")
        app.expect("steer")
        app.send(b"hadsbl r\x1b[D\x1b[C")
        app.pump(0.2)
        app.resize(16, 6)
        app.expect("Resize")
        app.resize(80, 28)
        app.expect("BEATFALL")
        app.finish()
    for mode, pattern in [("auto", rb"\x1b\[48;5;"), ("truecolor", rb"\x1b\[48;2;")]:
        with Terminal(binary, ["--demo", "--color", mode], {"NO_COLOR": "1"}) as app:
            app.expect("DEMO")
            app.pump(0.2)
            assert re.search(pattern, app.raw), f"Missing ball background colors in {mode}"
            if mode == "auto":
                assert not re.search(rb"\x1b\[(?:38|48);2;", app.raw), "Auto ignored NO_COLOR"
            app.finish(key=b"\x03")
    print("PASS demo controls, pause, resize, NO_COLOR modes, Q/Ctrl-C, terminal restoration")


def tone(seconds, frequency=1000):
    samples = [0.65 * math.sin(math.tau * frequency * i / 48000) for i in range(round(48000 * seconds))]
    return struct.pack(f"<{len(samples)}f", *samples)


def stdin_checks(binary):
    with Terminal(binary, ["--stdin"], pcm=True) as app:
        app.expect("STDIN")
        quiet = app.screen.bars()
        packet = tone(0.05)
        for _ in range(20):
            # Split a sample across writes as real pipes are free to do.
            app.write_pcm(packet[:3])
            app.write_pcm(packet[3:])
            app.pump(0.05)
        assert app.screen.bars() > quiet + 15, "PCM did not drive the spectrum"
        app.send(b"p")
        app.expect("PAUSED")
        frozen = app.screen.field()
        app.write_pcm(struct.pack("<4f", float("nan"), float("inf"), -100, 100))
        for _ in range(6):
            app.write_pcm(packet)
        app.write_pcm(b"\0\0\0")
        app.close_pcm()
        app.expect("trailing bytes")
        assert app.screen.field() == frozen, "Paused PCM changed the animation"
        app.send(b"p")
        app.until(lambda: app.screen.bars() <= quiet + 10, "spectrum decay after EOF")
        app.finish()
    with Terminal(binary, ["--stdin"], pcm=True) as app:
        app.expect("STDIN")
        # Keep the writer open and idle: quitting must interrupt the reader.
        app.finish(key=b"\x1b")
    print("PASS PCM response, fragmented samples, paused drain, truncated EOF, idle-pipe exit")


def stop(process):
    if process is not None:
        if process.poll() is None:
            process.terminate()
        try:
            process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=3)


def pulse_checks(binary):
    if sys.platform != "linux" or os.geteuid() == 0:
        raise SystemExit("--pulse requires Linux and a non-root user")
    for command in ("pulseaudio", "pactl", "pacat", "parec"):
        if not shutil.which(command):
            raise SystemExit(f"--pulse requires {command}")
    with tempfile.TemporaryDirectory(prefix="cwb-pulse-") as directory:
        root = Path(directory)
        runtime = root / "runtime"
        runtime.mkdir(mode=0o700)
        config = root / "config"
        config.mkdir(mode=0o700)
        client_config = config / "client.conf"
        client_config.write_text("autospawn = no\n", encoding="utf-8")
        env = dict(os.environ, HOME=str(root), XDG_CONFIG_HOME=str(config), LC_ALL="C.UTF-8",
                   PULSE_SERVER=f"unix:{root}/native", PULSE_COOKIE=str(root / "cookie"),
                   PULSE_CLIENTCONFIG=str(client_config), PULSE_RUNTIME_PATH=str(runtime),
                   PULSE_STATE_PATH=str(root / "state"), XDG_RUNTIME_DIR=str(runtime))

        def pactl(*args):
            return subprocess.run(["pactl", *args], env=env, check=True,
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=5).stdout

        daemon = player = None
        with (root / "server.log").open("w+b") as log:
            try:
                daemon = subprocess.Popen([
                    "pulseaudio", "--daemonize=no", "--use-pid-file=no", "--exit-idle-time=-1",
                    "--disable-shm=yes", "-n", "--log-target=stderr",
                    f"--load=module-native-protocol-unix socket={root}/native auth-anonymous=1",
                    "--load=module-null-sink sink_name=cwb_smoke rate=48000 channels=1",
                ], env=env, stdout=subprocess.DEVNULL, stderr=log)
                deadline = time.monotonic() + 8
                while not (root / "native").exists():
                    if daemon.poll() is not None or time.monotonic() > deadline:
                        log.seek(0)
                        raise AssertionError("Isolated PulseAudio failed: " + log.read().decode(errors="replace"))
                    time.sleep(0.05)
                pactl("set-default-sink", "cwb_smoke")
                pactl("load-module", "module-remap-source", "master=cwb_smoke.monitor",
                      "source_name=cwb_input")
                pactl("set-default-source", "cwb_input")
                sources = json.loads(pactl("--format=json", "list", "sources"))
                monitor = next(s for s in sources if s["name"] == "cwb_smoke.monitor")
                listing = subprocess.run([binary, "--list-sources"], env=env, check=True,
                                         capture_output=True, timeout=5).stdout.decode()
                assert "cwb_smoke.monitor" in listing and "cwb_input" in listing
                baseline_outputs = {stream["index"] for stream in
                                    json.loads(pactl("--format=json", "list", "source-outputs"))}
                with Terminal(binary, [], env) as app:
                    app.expect("PLAYBACK")
                    app.pump(0.6)
                    quiet = app.screen.bars()
                    pcm_file = root / "tone.f32le"
                    pcm_file.write_bytes(tone(4))
                    with pcm_file.open("rb") as pcm:
                        player = subprocess.Popen([
                            "pacat", "--playback", "--raw", "--format=float32le", "--rate=48000",
                            "--channels=1", "--device=cwb_smoke", "--latency-msec=30",
                        ], stdin=pcm, stdout=subprocess.DEVNULL, stderr=log, env=env)
                    app.until(lambda: app.screen.bars() > quiet + 15, "real monitor audio response")
                    app.finish()
                remaining_outputs = {stream["index"] for stream in
                                     json.loads(pactl("--format=json", "list", "source-outputs"))}
                assert remaining_outputs == baseline_outputs, "Leaked capture client"
                with Terminal(binary, ["--source", str(monitor["index"])], env) as app:
                    app.expect("PLAYBACK")
                    app.finish()
                with Terminal(binary, ["--microphone", "--source", "cwb_smoke.monitor"], env) as app:
                    app.pump(0.3)
                    app.finish(key=None, expected=1, entered=False)
                    assert b"not an input" in app.raw
                with Terminal(binary, ["--microphone"], env) as app:
                    app.expect("INPUT")
                    app.send(b"p")
                    app.expect("PAUSED")
                    stop(player)
                    player = None
                    stop(daemon)
                    daemon = None
                    app.until(lambda: "ERROR" in app.screen.text() or "ENDED" in app.screen.text(),
                              "capture disconnect status")
                    app.finish()
            finally:
                stop(player)
                stop(daemon)
    print("PASS isolated Linux playback, source selection/classification, disconnect, child cleanup")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, help="path to the built native executable")
    parser.add_argument("--pulse", action="store_true", help="also exercise an isolated real PulseAudio server")
    parser.add_argument("--snapshot", help="save the decoded demo screen as UTF-8 text")
    args = parser.parse_args()
    binary = str(Path(args.binary).resolve(strict=True))
    demo_checks(binary, args.snapshot)
    stdin_checks(binary)
    if args.pulse:
        pulse_checks(binary)


if __name__ == "__main__":
    main()
