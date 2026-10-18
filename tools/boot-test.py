#!/usr/bin/env python3
"""Boot ruOS in QEMU and drive the console like a user would.

Checks: boot reaches a shell, ruosd answers, a read-only tool runs without
asking, an approved command runs, a declined command does not run, the audit
log records both decisions, and poweroff shuts the VM down.

Runs with the offline provider (no config passed), so it needs no API key.
Usage: tools/boot-test.py <bzImage> <initramfs>
"""
import os
import re
import select
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))


class Console:
    def __init__(self, kernel, initrd):
        self.proc = subprocess.Popen(
            [os.path.join(HERE, "run-qemu.sh"), kernel, initrd],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        self.buf = b""
        self.log = open(os.environ.get("BOOT_LOG", "/dev/null"), "wb")

    def expect(self, pattern, timeout=120):
        rx = re.compile(pattern.encode())
        deadline = time.time() + timeout
        while True:
            m = rx.search(self.buf)
            if m:
                self.buf = self.buf[m.end():]
                return m
            left = deadline - time.time()
            if left <= 0:
                tail = self.buf[-2000:].decode(errors="replace")
                raise AssertionError(f"timed out waiting for {pattern!r}; last output:\n{tail}")
            ready, _, _ = select.select([self.proc.stdout], [], [], min(left, 1))
            if ready:
                chunk = os.read(self.proc.stdout.fileno(), 65536)
                if not chunk:
                    raise AssertionError(f"QEMU exited while waiting for {pattern!r}")
                self.log.write(chunk)
                self.log.flush()
                self.buf += chunk

    def send(self, line):
        self.proc.stdin.write(line.encode() + b"\n")
        self.proc.stdin.flush()


def step(name):
    print(f"ok - {name}", flush=True)


def main():
    kernel, initrd = sys.argv[1], sys.argv[2]
    c = Console(kernel, initrd)
    PROMPT = r"root@ruos:[^#]*# "
    try:
        c.expect(r"ai <request>", timeout=300)
        c.expect(PROMPT)
        step("boots to a shell")

        c.send("ai system info")
        c.expect(r"kernel: Linux version 6\.18\.\d+-ruos")
        c.expect(PROMPT)
        step("read-only tool runs without approval")

        c.send("ai 'run echo $((6*7))'")
        c.expect(r"allow\? \[y/N\]")
        c.send("y")
        c.expect(r"│ 42")
        c.expect(PROMPT)
        step("approved command runs")

        c.send("ai 'run touch /tmp/declined'")
        c.expect(r"allow\? \[y/N\]")
        c.send("n")
        c.expect(r"Not done\.")
        c.expect(PROMPT)
        c.send("test -e /tmp/declined && echo FILE-EXISTS || echo FILE-ABSENT")
        c.expect(r"\nFILE-ABSENT")
        c.expect(PROMPT)
        step("declined command does not run")

        c.send("grep -c '\"approved\":false' /var/log/ruos-audit.log")
        c.expect(r"\n1\r?\n")
        c.expect(PROMPT)
        step("audit log records decisions")

        c.send("poweroff")
        c.proc.wait(timeout=60)
        step("poweroff stops the VM")
    finally:
        if c.proc.poll() is None:
            c.proc.kill()


if __name__ == "__main__":
    main()
