# Install guide for AI agents (Cline, Claude Code, etc.)

This is a **remote** MCP server. There is nothing to clone, build or run locally.

Add this server to the client's MCP settings as a Streamable HTTP server:

- URL: `https://arete.ai.kr/connect/mcp`
- Transport: Streamable HTTP (`"type": "streamableHttp"` in Cline, `--transport http` in Claude Code)
- Authentication: none required. An optional free account key from https://arete.ai.kr/connect raises the limits; pass it as `Authorization: Bearer <key>`.

Example (Cline `cline_mcp_settings.json`):

```json
{
  "mcpServers": {
    "arete": {
      "type": "streamableHttp",
      "url": "https://arete.ai.kr/connect/mcp"
    }
  }
}
```

Example (Claude Code):

```
claude mcp add --transport http arete https://arete.ai.kr/connect/mcp
```

After connecting, call `search` with the user's task in plain words (Korean or English), e.g. "회의록 요약" or "make subtitles for a YouTube video".
