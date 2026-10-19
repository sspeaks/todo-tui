# ADR-0001: Codegen pipeline and runtime path deviations

- Status: accepted
- Date: 2026-10-18
- Deviates from: design 4.7 (codegen), 7.1 (runtime locations)

## Decisions

**D1. Rust codegen in `build.rs`.** `crates/todo-proto/build.rs` generates the Rust stubs
with `tonic-prost-build` and the local `protoc`, and also emits a file descriptor set (for
`prost-reflect`). Buf stays the tool for `buf lint`, `buf breaking` and TypeScript codegen
(`buf generate` with local `protoc-gen-es`). No generated code is committed. *Why:* plain
`cargo build` works without running Buf first, and the Rust stubs can never drift from
`proto/`.

**D7. `daemon.lock` lives in the data dir, not the runtime dir.** The socket stays in the
runtime dir. *Why:* the runtime dir depends on `TMPDIR`/`XDG_RUNTIME_DIR`; if two processes
resolve it differently they could each take "the" lock and open the same database. The lock
sits next to the database it protects, so one database always means one daemon.

**D8. Long-path fallback is per home.** When the socket path exceeds 100 bytes and
`TODO_TUI_HOME` is set, the socket goes to `/tmp/todo-tui-$UID/h-<sha8(home)>/daemon.sock`,
where `sha8` is the first 8 hex digits of the SHA-256 of the home path. Without
`TODO_TUI_HOME` the 7.1 fallback `/tmp/todo-tui-$UID/` is unchanged. *Why:* parallel test
homes with long paths would otherwise share one socket.
