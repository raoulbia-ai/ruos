# ruos

This repo holds one component: **[`mcp/`](./mcp/)**, `ruos-mcp`, a small
computer-use MCP server written in Rust. It lets an MCP client (for example
Claude Code) drive a Linux X11 desktop over stdio-over-SSH: screenshots,
mouse and keyboard through `xdotool`, and shell commands.

Most tools need only an X session and `xdotool`. `system_action` and
`desktop_resolution` also need a desktop-side executor on `127.0.0.1:17870`,
and without one they return "no response from the desktop executor". See [`mcp/README.md`](./mcp/README.md)
for the tool list, build steps and client setup.

```bash
cd mcp && cargo build --release      # → target/release/ruos-mcp
claude mcp add ruos -- ssh you@your-desktop \
  'DISPLAY=:0 XAUTHORITY=/path/to/Xauthority /usr/local/bin/ruos-mcp'
```

## License

MIT — see [LICENSE](./LICENSE).
