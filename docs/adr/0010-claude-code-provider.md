# ADR-0010: Claude through the person's Claude Code CLI, on request

- Status: accepted
- Date: 2026-11-12
- Amends: design 6.1 (the default provider), 6.2 (`refine`'s trigger), 6.3 (the
  pipeline as first built), 8.2 (`[assistant]`); the first milestone of P3

## Context

todo-tui is a terminal todo manager whose Rust daemon owns the data and whose
clients (an Ink TUI, the `todo` CLI, an MCP server, a phone app) are thin; its
design is [`docs/design.md`](../design.md). Section 6 plans an optional assistant:
Claude proposes, the person decides, and everything Claude makes is a Suggestion
(3.11) in the same pane as Jira's fallout. It names two providers, the Claude API
(`api`, the default) and the person's own Claude Code CLI (`claude_code`), and has
`refine` run on every capture by default.

The person this is built for has Claude Code installed and logged in, and no
Anthropic API key on the machine. A key would mean a second account, a second
bill, and a secret to store.

## Decision

**The first provider is the Claude Code CLI, and refine runs only on request.**

- **Provider**: the daemon runs `claude -p --output-format json --json-schema
  <schema> --system-prompt <prompt> --tools "" --strict-mcp-config
  --setting-sources "" --no-session-persistence [--model <model>]`, the context on
  stdin. No tools, no MCP servers, no settings and no saved session, so what the
  daemon sends is all Claude sees. The daemon never reads, copies or refreshes
  Claude Code's credentials; the CLI uses whatever it is logged into, billed to that
  plan. A timeout (120 s by default) kills it. Its JSON envelope gives the answer
  (`structured_output`) and the usage (tokens, cached tokens, cost, time), which each
  job records.
- **On request only**: `space r` and `:refine` in the TUI, and `todo refine` on the
  command line, ask for one Item or the marked ones. Nothing runs on capture.
- **Refine** proposes a clearer title, the due date, priority, labels and waiting-on
  the text supports, and names a likely duplicate among up to 20 Items whose title
  shares a word (found with the store's full-text index). The answer is validated:
  invalid values, unchanged fields and fields applying would refuse (a linked Item's
  tracker-only fields) are dropped. Each patch holds the value Claude saw, so an edit
  made meanwhile makes it stale. Claude's patches start ticked in the pane.
- **Config**: `[assistant] provider = "claude_code"` turns it on; `model`,
  `claude_bin`, `style_notes`, `include_private_notes` (false: private notes are
  never sent unless allowed), `timeout_seconds`.

## Alternatives considered

- **The Claude API first** (design 6.1's default). Faster, with prompt caching and
  per-call model choice, but it needs a key this machine does not have. It stays
  planned: the provider is a trait, and `api` is a second implementation.
- **Auto-refine on capture** (design 6.2's default). Every quick capture would cost
  a few seconds of CLI time and tokens, for proposals the person may not want. On
  request first; auto can be a config value later, debounced and batched as 6.3
  describes.
- **Give the CLI the MCP server's read-only tools** (design 6.1's note). Claude could
  then look things up itself, but it would see more than the context the daemon
  chose, and each lookup is another round trip. The context is built by the daemon.

## Consequences

- Refine works wherever the person's `claude` works, offline from Anthropic's point
  of view only in that the request fails and says so.
- The CLI's flags and envelope can change between Claude Code versions: the parser
  is lenient (an older text answer is read too), errors say what to do, and a fixture
  of the envelope is kept in the tests. Tests and smoke never call Claude: the daemon
  takes a stand-in, and smoke a stub `claude` script.
- `merge` (a duplicate folded into another Item) waits for a merge op; until then
  the duplicate is named in the Suggestion's rationale.
