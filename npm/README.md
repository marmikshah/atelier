# @marmikshah/atelier

[Atelier](https://github.com/marmikshah/atelier) is an offline, headless
pixel-art editor for AI agents. This package starts it as an MCP server, so
any MCP client can use it with nothing else installed:

```json
{
  "mcpServers": {
    "atelier": { "command": "npx", "args": ["-y", "@marmikshah/atelier"] }
  }
}
```

The first start downloads the Atelier binary for your machine (macOS or
Linux, x86-64 or arm64) from the matching
[GitHub Release](https://github.com/marmikshah/atelier/releases), checks its
SHA-256 sum, and caches it. Later starts run the cached copy.

For Claude Code and Codex, the
[one-line installer](https://github.com/marmikshah/atelier#install) is the
better fit: it also installs Atelier's drawing guides and the `atelier`
command.

To remove it, delete the entry from your client's config. Cached downloads
live in `~/Library/Caches/atelier` on macOS and `~/.cache/atelier` on Linux;
`npx -y @marmikshah/atelier uninstall` deletes them.
