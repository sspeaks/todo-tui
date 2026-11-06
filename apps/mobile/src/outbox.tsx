// Adds made while the computer cannot be reached: kept on the phone (by
// the server they are for) and sent in order once the store is live again.
// Each keeps its request id, so one that did reach the daemon before the
// connection dropped is not captured twice (the daemon remembers request
// ids for 24 hours), and its reference time, so "^tom" means the day after
// it was written, not after it was sent.
import AsyncStorage from "@react-native-async-storage/async-storage";
import { CreatedVia, captureItem, isUnreachable } from "@todo-tui/client/portable";
import { useClients, useConnection, useLiveStore } from "@todo-tui/client/react";
import { createContext, type ReactNode, useCallback, useContext, useEffect, useRef, useState } from "react";
import { useSnack } from "./snack";

export interface Pending {
  requestId: string;
  /** The capture text, tokens included. */
  text: string;
  /** What the list shows while it waits. */
  title: string;
  /** When it was written (ISO), the capture's reference time. */
  at: string;
  /** The server it is for. */
  url: string;
}

const KEY = "todo-tui.outbox";

interface OutboxApi {
  /** Waiting for this server, oldest first. */
  pending: readonly Pending[];
  add(p: Omit<Pending, "url">): Promise<void>;
}

const Ctx = createContext<OutboxApi | null>(null);

export function useOutbox(): OutboxApi {
  const api = useContext(Ctx);
  if (!api) throw new Error("useOutbox outside <OutboxProvider>");
  return api;
}

async function load(): Promise<Pending[]> {
  try {
    const raw = await AsyncStorage.getItem(KEY);
    const all = raw ? (JSON.parse(raw) as unknown) : [];
    return Array.isArray(all) ? (all as Pending[]).filter((p) => typeof p?.requestId === "string" && typeof p.text === "string") : [];
  } catch {
    return [];
  }
}

const save = (all: readonly Pending[]) => AsyncStorage.setItem(KEY, JSON.stringify(all));

export function OutboxProvider({ url, children }: { url: string; children: ReactNode }) {
  const clients = useClients();
  const store = useLiveStore();
  const snack = useSnack();
  const live = useConnection().state === "live";
  /** Every server's entries, as stored. */
  const [all, setAll] = useState<Pending[]>([]);
  const allRef = useRef(all);
  const sending = useRef(false);
  /** Bumped after a send that met no outage, to pick up what was added meanwhile. */
  const [again, setAgain] = useState(0);

  const replace = useCallback(async (next: Pending[]) => {
    allRef.current = next;
    setAll(next);
    await save(next);
  }, []);

  useEffect(() => {
    void load().then((loaded) => {
      allRef.current = loaded;
      setAll(loaded);
    });
  }, []);

  const add = useCallback(async (p: Omit<Pending, "url">) => replace([...allRef.current, { ...p, url }]), [replace, url]);

  // Live again: send what waits, oldest first, stopping at the first outage.
  const pendingCount = all.filter((p) => p.url === url).length;
  useEffect(() => {
    if (!live || pendingCount === 0 || sending.current) return;
    sending.current = true;
    let outage = false;
    void (async () => {
      try {
        for (const p of allRef.current.filter((x) => x.url === url)) {
          try {
            const res = await captureItem(clients.items, p.text, {
              createdVia: CreatedVia.MOBILE,
              requestId: p.requestId,
              referenceTime: new Date(p.at),
              timeoutMs: 15_000,
            });
            if (res.item && res.result) store.applyItems([res.item], res.result.revision);
          } catch (err) {
            if (isUnreachable(err)) {
              outage = true;
              return;
            }
            // Refused (it would be refused again): say so, and let it go.
            snack.show(`Couldn't add "${p.title}": ${err instanceof Error ? err.message : String(err)}`);
          }
          await replace(allRef.current.filter((x) => x.requestId !== p.requestId));
        }
        snack.show("Sent what you added offline");
      } finally {
        sending.current = false;
        if (!outage) setAgain((n) => n + 1);
      }
    })();
  }, [live, pendingCount, url, clients, store, snack, replace, again]);

  return <Ctx.Provider value={{ pending: all.filter((p) => p.url === url), add }}>{children}</Ctx.Provider>;
}
