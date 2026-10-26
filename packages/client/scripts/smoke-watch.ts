// Used by scripts/smoke.sh: connects (auto-spawning the daemon), subscribes
// from the ListItems revision, prints READY, then prints the first changed
// item as protobuf JSON and exits. With --list it prints every item instead,
// and with --view <seed> the items the seeded view lists, in its order.
// TODO_TUI_HOME / TODO_BIN come from the env.
import { toJsonString } from "@bufbuild/protobuf";
import { ConnectError, createClient } from "@connectrpc/connect";
import { ItemSchema } from "../src/gen/todo/v1/item_pb.js";
import { ViewService } from "../src/gen/todo/v1/view_service_pb.js";
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

async function listView(seed: string): Promise<number> {
  const conn = await connect();
  try {
    const { views } = await createClient(ViewService, conn.transport).listViews({});
    const view = views.find((v) => v.seed === seed);
    if (!view) throw new Error(`no view seeded from "${seed}"`);
    const { items } = await conn.items.listItems({ viewId: view.id, pageSize: 1000 });
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

function main(): Promise<number> {
  const view = process.argv.indexOf("--view");
  if (view >= 0) return listView(process.argv[view + 1] ?? "");
  return process.argv.includes("--list") ? list() : watch();
}

main().then(
  (code) => process.exit(code),
  (err: unknown) => {
    const reason = err instanceof ConnectError ? `${err.code}: ${err.rawMessage}` : String(err);
    process.stderr.write(`smoke-watch: ${reason}\n`);
    process.exit(1);
  },
);
