// Used by scripts/smoke.sh: connects (auto-spawning the daemon), subscribes
// from the ListItems revision, prints READY, then prints the first changed
// item as protobuf JSON and exits. With --list it prints every item instead.
// TODO_TUI_HOME / TODO_BIN come from the env.
import { toJsonString } from "@bufbuild/protobuf";
import { ConnectError } from "@connectrpc/connect";
import { ItemSchema } from "../src/gen/todo/v1/item_pb.js";
import { connect } from "../src/connect.js";

const timeoutMs = Number(process.env.SMOKE_TIMEOUT_MS ?? 15_000);

async function list(): Promise<number> {
  const conn = await connect();
  try {
    const { items } = await conn.items.listItems({ pageSize: 1000 });
    for (const item of items) process.stdout.write(`${toJsonString(ItemSchema, item)}\n`);
    return 0;
  } finally {
    conn.close();
  }
}

async function watch(): Promise<number> {
  const conn = await connect();
  const ac = new AbortController();
  const timer = setTimeout(() => ac.abort(new Error(`no item change within ${timeoutMs}ms`)), timeoutMs);
  try {
    const { revision } = await conn.items.listItems({});
    const stream = conn.watch.subscribe({ sinceRevision: revision }, { signal: ac.signal });
    for await (const res of stream) {
      const msg = res.message;
      if (msg.case === "caughtUp") {
        process.stdout.write("READY\n");
      } else if (msg.case === "change" && msg.value.resource.case === "item") {
        process.stdout.write(`${toJsonString(ItemSchema, msg.value.resource.value)}\n`);
        return 0;
      }
    }
    throw new Error("stream ended before an item change");
  } finally {
    clearTimeout(timer);
    ac.abort();
    conn.close();
  }
}

(process.argv.includes("--list") ? list() : watch()).then(
  (code) => process.exit(code),
  (err: unknown) => {
    const reason = err instanceof ConnectError ? `${err.code}: ${err.rawMessage}` : String(err);
    process.stderr.write(`smoke-watch: ${reason}\n`);
    process.exit(1);
  },
);
