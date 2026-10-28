import type { ItemsClient, LiveStoreOptions } from "@todo-tui/client";
import { render } from "ink";
import { expect, vi } from "vitest";
import { Tui } from "./App.js";
import { Controller, type ControllerOptions } from "./controller/controller.js";
import type { Timing } from "./controller/pipeline.js";
import { type FakeDaemonHandle, fakeDaemon } from "./fake.test-util.js";
import { virtualTerminal } from "./term.js";

export const stripAnsi = (s: string) => s.replace(/\x1b\[[0-9;]*m/g, "");

const mounted: (() => Promise<void>)[] = [];

/** Unmounts every renderTui and stops its store; for afterEach. */
export async function cleanupTui(): Promise<void> {
  await Promise.all(mounted.splice(0).map((stop) => stop()));
}

export interface TuiOptions {
  /** The terminal size (default 100×30); ink-testing-library would fix it at 100 columns and no rows. */
  columns?: number;
  rows?: number;
  daemon?: FakeDaemonHandle;
  now?: () => Date;
  store?: Partial<LiveStoreOptions>;
  timing?: Partial<Timing>;
  /** The ItemService client actions call (default: the daemon's), e.g. one that holds calls. */
  items?: (items: ItemsClient) => ItemsClient;
  /** E's editor and the state dir. */
  controller?: Pick<ControllerOptions, "editor" | "stateDir">;
}

/** The whole TUI over FakeDaemon in an off-screen terminal of a set size. */
export function renderTui(opts: TuiOptions = {}) {
  const fd = opts.daemon ?? fakeDaemon();
  const store = fd.store(opts.store);
  const clients = fd.clients();
  const items = opts.items ? opts.items(clients.items) : clients.items;
  const controller = new Controller({ store, items, ...(opts.now && { now: opts.now }), ...(opts.timing && { timing: opts.timing }), ...opts.controller });
  const term = virtualTerminal(opts.columns ?? 100, opts.rows ?? 30);
  store.start();
  const app = render(<Tui store={store} clients={clients} controller={controller} />, term.options);
  mounted.push(async () => {
    app.unmount();
    controller.stop();
    await store.stop();
  });
  const raw = () => term.stdout.lastFrame();
  const frame = () => stripAnsi(raw());
  const lines = () => frame().split("\n");
  return {
    ...fd,
    store,
    controller,
    ui: controller.ui,
    term,
    app,
    raw,
    frame,
    lines,
    /** The list row with the selection marker. */
    selectedLine: () => lines().find((l) => l.includes("›")),
    /** One stdin chunk, as one terminal read delivers it, through Ink's key parsing. */
    type: (chunk: string) => term.stdin.write(chunk),
    /** Keys straight to the controller ("jj<esc>"), resolved once handled. */
    press: (keys: string) => controller.press(keys),
    resize: (columns: number, rows: number) => term.stdout.resize(columns, rows),
    /** Waits until the frame (ANSI stripped) passes `check`. */
    waitFrame: (check: (frame: string) => void) => vi.waitFor(() => check(frame())),
    /** Waits for "● live" and the first rows. */
    live: () => vi.waitFor(() => expect(frame()).toMatch(/● live/)),
  };
}
