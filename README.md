# todo-tui

A keyboard-driven, local-first todo manager for the terminal. A Rust daemon owns all
state in SQLite and serves a gRPC API (`todo.v1`) over a Unix socket; an Ink TUI, the
`todo` CLI and an MCP server are clients. See [docs/design.md](docs/design.md).

Status: early development (P0 spike).

## Layout

- `proto/` — the `todo.v1` contract (Buf)
- `crates/` — Rust workspace: core, store, sync, jira, assistant, proto, daemon, cli
- `packages/client/` — shared TypeScript client (Connect-ES)
- `tui/`, `mcp/` — the Ink TUI and the MCP server

## Development

Requires Rust 1.97, Node 24, `protoc` on `PATH`, and corepack (`corepack enable`).

```sh
pnpm install
pnpm gen              # TypeScript codegen into packages/client/src/gen
pnpm lint:proto
pnpm typecheck
cargo build --workspace
cargo test --workspace
```

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT)
at your option.
