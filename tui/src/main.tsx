import { ClientKind, LiveStore, LiveStoreProvider, connect } from "@todo-tui/client";
import { render, renderToString } from "ink";
import { App } from "./App.js";
import { ItemList } from "./ItemList.js";
import { sampleItems } from "./sample.js";

const usage = `Usage: todo-tui [options]

Options:
  --smoke    render a sample list once and exit (checks the bundle)
  -h, --help show this help`;

async function main(args: string[]): Promise<number> {
  if (args.includes("-h") || args.includes("--help")) {
    console.log(usage);
    return 0;
  }
  if (args.includes("--smoke")) {
    const items = sampleItems();
    const list = <ItemList items={items} selectedId={items[0]?.id} fresh={new Set()} offset={0} height={items.length} />;
    console.log(renderToString(list, { columns: 60 }));
    return 0;
  }
  if (args.length) {
    console.error(`todo-tui: unknown argument ${args[0]}\n\n${usage}`);
    return 2;
  }

  const conn = await connect({ clientKind: ClientKind.INTERACTIVE });
  const store = new LiveStore({ transport: conn.transport, ensureDaemon: conn.ensureDaemon });
  store.start();
  try {
    await render(
      <LiveStoreProvider store={store}>
        <App />
      </LiveStoreProvider>,
    ).waitUntilExit();
  } finally {
    await store.stop();
    conn.close();
  }
  return 0;
}

main(process.argv.slice(2)).then(
  (code) => process.exit(code),
  (err: unknown) => {
    console.error(`todo-tui: ${err instanceof Error ? err.message : String(err)}`);
    process.exit(1);
  },
);
