import { ClientKind, FakeDaemon, LiveStore, connect, createClients } from "@todo-tui/client";
import { render } from "ink";
import { Tui } from "./App.js";
import { Controller } from "./controller/controller.js";
import { sampleItems } from "./sample.js";
import { virtualTerminal } from "./term.js";

const usage = `Usage: todo-tui [options]

Options:
  --smoke    render the shell once over sample data and exit (checks the bundle)
  -h, --help show this help`;

/** Renders the shell off-screen over an in-memory daemon with sample Items, prints the frame, and exits. */
async function smoke(): Promise<number> {
  const daemon = new FakeDaemon(sampleItems(new Date()));
  const store = new LiveStore({ transport: daemon.transport, debounceMs: 5 });
  const controller = new Controller({ store });
  const term = virtualTerminal(120, 16);
  store.start();
  const app = render(<Tui store={store} clients={createClients(daemon.transport)} controller={controller} />, term.options);
  try {
    const deadline = Date.now() + 5_000;
    while (!/● live[\s\S]*Call dentist|Call dentist[\s\S]*● live/.test(term.stdout.lastFrame())) {
      if (Date.now() > deadline) throw new Error("the shell did not render the sample Items");
      await new Promise((r) => setTimeout(r, 10));
    }
    console.log(term.stdout.lastFrame());
    return 0;
  } finally {
    app.unmount();
    controller.stop();
    await store.stop();
  }
}

async function main(args: string[]): Promise<number> {
  if (args.includes("-h") || args.includes("--help")) {
    console.log(usage);
    return 0;
  }
  if (args.includes("--smoke")) return smoke();
  if (args.length) {
    console.error(`todo-tui: unknown argument ${args[0]}\n\n${usage}`);
    return 2;
  }

  const conn = await connect({ clientKind: ClientKind.INTERACTIVE });
  const store = new LiveStore({ transport: conn.transport, ensureDaemon: conn.ensureDaemon });
  const controller = new Controller({ store });
  store.start();
  try {
    // ctrl-c is a key: it quits in normal mode and cancels in the others.
    await render(<Tui store={store} clients={conn} controller={controller} />, { alternateScreen: true, exitOnCtrlC: false }).waitUntilExit();
  } finally {
    controller.stop();
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
