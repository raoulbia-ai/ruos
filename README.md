# ruOS

A small Linux-based operating system with an AI agent built in as a system
service. ruOS is built from source here: a Linux LTS kernel with our own config,
our own PID 1, and `ruosd`, a system service that gives a model (Claude) tools
to inspect and operate the machine, with the user approving anything that
changes it.

Status: first boot milestone. It boots in QEMU to a shell, and the agent works
end to end with the offline provider. The Claude provider is implemented but
has not been tested against the live API yet (see [Known gaps](#known-gaps)).

> The earlier ruOS design (an agentic GNOME desktop on Ubuntu, with the
> `mcp/` computer-use server and `skills/`) is described in
> [docs/agentic-desktop.md](docs/agentic-desktop.md).

## What runs

```
 QEMU x86_64 (virtio-net, virtio-rng, serial console)
 └─ Linux 6.18 LTS            kernel/ruos.config on top of x86_64_defconfig
    └─ /init = ruos-init      PID 1: mounts, hostname, network, supervision, shutdown
       ├─ ruosd               AI system service on /run/ruosd.sock (root only, 0600)
       │   ├─ provider        Claude API, or offline rule-based fallback
       │   └─ tools           run_command (needs approval), read_file, list_dir, system_info
       └─ /bin/sh (busybox)   login shell on the console
           └─ ai              client: sends requests to ruosd, shows approval prompts
```

- **ruos-init** (`userland/src/bin/ruos-init.rs`) mounts `/proc`, `/sys`,
  `/dev`, `/run`, `/tmp`, configures QEMU user networking (10.0.2.15, gateway
  10.0.2.2, DNS 10.0.2.3), starts `ruosd` and the console shell, restarts
  either if it exits, reaps orphans, and handles `poweroff`/`reboot`/`halt`.
- **ruosd** (`userland/src/bin/ruosd.rs`, `userland/src/agent.rs`) runs the
  tool-use loop. Each connection has its own conversation. Read-only tools run
  without asking; `run_command` asks the user every time. Every tool call and
  approval decision is appended to `/var/log/ruos-audit.log`; service logs go to
  `/var/log/ruosd.log`.
- **ai** (`userland/src/bin/ai.rs`): `ai <request>` for one request, `ai` for
  an interactive session (`/reset` clears history).

## Build and run

Needs: `make`, a C toolchain, `flex`, `bison`, `bc`, `libelf-dev`,
`libssl-dev`, `cpio`, `curl`, Rust with the `x86_64-unknown-linux-musl` target,
`musl-tools`, a static busybox (`busybox-static` on Debian/Ubuntu, at
`/bin/busybox`), and `qemu-system-x86`.

```bash
rustup target add x86_64-unknown-linux-musl
make            # downloads + verifies the kernel tarball, builds kernel and initramfs into build/
make run        # boots on this terminal; Ctrl-A X quits QEMU
make test       # unit tests + scripted boot test (no API key needed)
```

Without an API key, `ruosd` uses the offline provider, which understands
`list files [path]`, `system info`, `read <path>`, and `run <command>`.

To use Claude, put settings in a file outside the repo and pass it at boot. The
file is handed to the guest through QEMU `fw_cfg`; it is not written into the
image.

```bash
cat > ~/ruos.env <<'EOF'
ANTHROPIC_API_KEY=sk-ant-...
# RUOS_MODEL=claude-opus-5-5
# RUOS_EFFORT=medium
EOF
make run CONFIG=~/ruos.env
```

Then, at the ruOS prompt:

```
root@ruos:~# ai what is using the most memory?
root@ruos:~# ai create a file /root/notes.txt with today's date
ruosd wants to run: date > /root/notes.txt
  reason: Write today's date to the file you asked for.
  allow? [y/N]
```

## Settings

`ruosd` reads `KEY=VALUE` settings from `/etc/ruos/ruosd.conf` (in the image),
then the fw_cfg file, then its environment; later sources win.

| Key | Default | Meaning |
|-----|---------|---------|
| `ANTHROPIC_API_KEY` | none | Claude API key. If set, the Claude provider is used. |
| `RUOS_PROVIDER` | `anthropic` if a key is set, else `offline` | Force a provider. |
| `RUOS_MODEL` | `claude-opus-5-5` | Model ID. |
| `RUOS_EFFORT` | `medium` | `low` / `medium` / `high` / `xhigh` / `max`. |
| `ANTHROPIC_BASE_URL` | `https://api.anthropic.com` | API endpoint. |
| `HTTPS_PROXY` | none | HTTP proxy for API traffic. |

Requests use adaptive thinking (the model default), `fallbacks: "default"` so
a safety decline is retried on Anthropic's recommended fallback model, and
prompt caching. A refusal or a cut-off response is reported and that request is
dropped from the conversation, so later requests still work.

## Repository layout

```
kernel/       ruos.config (config fragment), VERSION (pinned version + sha256)
userland/     Rust: ruos-init, ruosd, ai, and the shared library they use
rootfs/       files copied into the root filesystem (/etc)
tools/        mkinitramfs.sh, run-qemu.sh, boot-test.py
docs/         earlier agentic-desktop design
mcp/, skills/ earlier agentic-desktop components
```

## Known gaps

- **Claude provider not tested live.** The request shape is unit-tested, but
  no request has been sent from a booted VM to the real API yet. TLS uses the
  built-in Mozilla root set (`webpki-roots`), so a TLS-intercepting proxy will
  fail certificate checks.
- **Everything runs as root**, including tool commands. There is no user
  separation or sandbox yet. The kernel has Landlock, seccomp and user
  namespaces enabled so `run_command` can be confined next.
- **No persistent storage**: the root filesystem is an initramfs in RAM; all
  changes are lost at power off.
- **Busybox comes from the build host**, not built from source.
- **Network is static** and assumes QEMU user networking. No DHCP.
- **No streaming**: the agent's answer appears when the model's turn completes.
- **Only x86_64 under QEMU** has been tested. No real hardware.

## License

MIT — see [LICENSE](./LICENSE).
