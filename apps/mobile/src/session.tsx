// The paired daemon. Once paired, the session is ready at once, connected
// or not: the store reconnects on its own, adding works offline (the
// outbox), and the hello (GetServerInfo: the token and the API version)
// runs in the background, retried 2 s after a failure and doubling to
// 30 s while the app is not in the background. A refused token unpairs.
// The store is dropped in the background (iOS suspends its stream anyway)
// and made afresh on return, with the session still ready, so the screens
// (and where you were) stay mounted. A new store shows what the last one
// showed until the computer answers: on return, the old store's rows; at
// launch, the cache kept on the phone (./cache).
import {
  ClientKind,
  type GetServerInfoResponse,
  LiveStore,
  type Pairing,
  REMOTE_API_MINOR,
  type StoreCache,
  type TodoClients,
  type Transport,
  connectRemote,
  createClients,
  createRemoteTransport,
} from "@todo-tui/client/portable";
import { fetch as expoFetch } from "expo/fetch";
import { createContext, type ReactNode, useCallback, useContext, useEffect, useRef, useState } from "react";
import { AppState } from "react-native";
import { clearCache, loadCache } from "./cache";
import { describeError, isUnauthenticated } from "./errors";
import { clearPairing, loadPairing, savePairing } from "./pairing";

export const APP_VERSION = "0.1.0";

/** How the hello went: the daemon's version, or why it has not answered. */
export type Link =
  | { state: "checking" }
  | { state: "ok"; serverInfo: GetServerInfoResponse }
  | { state: "unreachable"; message: string }
  | { state: "too-old"; message: string };

export interface Conn extends TodoClients {
  transport: Transport;
}

export type Session =
  | { state: "loading" }
  | { state: "unpaired"; message?: string }
  | { state: "ready"; pairing: Pairing; conn: Conn; store: LiveStore; link: Link };

export interface SessionApi {
  session: Session;
  /** Checks the pairing against the daemon, then keeps it. Rejects with a message to show. */
  pair(p: Pairing): Promise<void>;
  unpair(message?: string): Promise<void>;
  /** Says hello again now. */
  retry(): void;
}

/** Exported for tests, which provide a session of their own. */
export const SessionContext = createContext<SessionApi | null>(null);
const Ctx = SessionContext;

export function useSession(): SessionApi {
  const api = useContext(Ctx);
  if (!api) throw new Error("useSession outside <SessionProvider>");
  return api;
}

const fetch = expoFetch as unknown as typeof globalThis.fetch;

/** A started store on `conn`, showing `cache` until it loads: it reconnects with backoff on its own. */
function storeOf(conn: Conn, cache?: StoreCache): LiveStore {
  const store = new LiveStore({ transport: conn.transport, ...(cache && { cache }) });
  store.start();
  return store;
}

function connOf(p: Pairing): Conn {
  const transport = createRemoteTransport({ baseUrl: p.url, token: p.token, fetch, timeoutMs: 15_000 });
  return { ...createClients(transport), transport };
}

export function SessionProvider({ children }: { children: ReactNode }) {
  const [session, setState] = useState<Session>({ state: "loading" });
  /** The session as last set, for callbacks that outlive a render. */
  const current = useRef<Session>(session);
  const setSession = useCallback((s: Session) => {
    current.current = s;
    setState(s);
  }, []);
  /** Bumped by every hello and every unpair, so one that lost the race is dropped. */
  const attempt = useRef(0);
  const retryTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const failures = useRef(0);
  /** The app has been in the background since the store was made: on return it is made afresh. */
  const away = useRef(false);
  const clearRetry = useCallback(() => {
    if (retryTimer.current !== undefined) clearTimeout(retryTimer.current);
    retryTimer.current = undefined;
  }, []);

  const setLink = useCallback(
    (link: Link) => {
      const s = current.current;
      if (s.state === "ready") setSession({ ...s, link });
    },
    [setSession],
  );

  const unpair = useCallback(
    async (message?: string) => {
      attempt.current++;
      clearRetry();
      const s = current.current;
      if (s.state === "ready") void s.store.stop();
      await Promise.all([clearPairing(), clearCache().catch(() => {})]);
      setSession(message === undefined ? { state: "unpaired" } : { state: "unpaired", message });
    },
    [clearRetry, setSession],
  );

  const hello = useCallback(
    async function hello(): Promise<void> {
      clearRetry();
      const s = current.current;
      if (s.state !== "ready") return;
      const mine = ++attempt.current;
      try {
        const info = await s.conn.system.getServerInfo(
          { clientKind: ClientKind.MOBILE, clientVersion: APP_VERSION, requiredApiMinor: REMOTE_API_MINOR },
          { timeoutMs: 15_000 },
        );
        if (mine !== attempt.current) return;
        failures.current = 0;
        setLink(
          info.apiMinor < REMOTE_API_MINOR
            ? {
                state: "too-old",
                message: `The todo on your computer is too old (API ${info.apiMinor}; this app needs ${REMOTE_API_MINOR}). Update it and run \`todo daemon stop\`.`,
              }
            : { state: "ok", serverInfo: info },
        );
      } catch (err) {
        if (mine !== attempt.current) return;
        if (isUnauthenticated(err)) return unpair(describeError(err));
        setLink({ state: "unreachable", message: describeError(err) });
        const delay = Math.min(30_000, 2_000 * 2 ** failures.current++);
        retryTimer.current = setTimeout(() => {
          if (AppState.currentState !== "background") void hello();
        }, delay);
      }
    },
    [clearRetry, setLink, unpair],
  );

  /** Ready on `p` at once (showing `cache` until the store loads), saying hello in the background unless `link` is already ok. */
  const start = useCallback(
    (p: Pairing, link: Link = { state: "checking" }, cache?: StoreCache) => {
      const conn = connOf(p);
      setSession({ state: "ready", pairing: p, conn, store: storeOf(conn, cache), link });
      failures.current = 0;
      if (link.state !== "ok") void hello();
    },
    [setSession, hello],
  );

  useEffect(() => {
    void Promise.all([loadPairing(), loadCache()]).then(([p, kept]) =>
      p ? start(p, undefined, kept?.url === p.url ? kept.cache : undefined) : setSession({ state: "unpaired" }),
    );
    const sub = AppState.addEventListener("change", (state) => {
      const s = current.current;
      if (s.state !== "ready") return;
      // Only back from the background: back from inactive (Control Center, a call's banner) the stream never stopped.
      if (state === "active" && away.current) {
        away.current = false;
        void s.store.stop();
        setSession({ ...s, store: storeOf(s.conn, s.store.toCache()) });
        failures.current = 0;
        void hello();
      } else if (state === "background") {
        away.current = true;
        clearRetry();
        void s.store.stop();
      }
    });
    return () => {
      sub.remove();
      clearRetry();
      const s = current.current;
      if (s.state === "ready") void s.store.stop();
    };
  }, [start, hello, clearRetry, setSession]);

  const pair = useCallback(
    async (p: Pairing) => {
      let serverInfo: GetServerInfoResponse;
      try {
        serverInfo = (await connectRemote({ baseUrl: p.url, token: p.token, fetch, timeoutMs: 15_000, clientVersion: APP_VERSION })).serverInfo;
      } catch (err) {
        throw new Error(describeError(err));
      }
      await savePairing(p);
      attempt.current++;
      clearRetry();
      const s = current.current;
      if (s.state === "ready") void s.store.stop();
      start(p, { state: "ok", serverInfo });
    },
    [start, clearRetry],
  );

  const retry = useCallback(() => {
    failures.current = 0;
    void hello();
  }, [hello]);

  return <Ctx.Provider value={{ session, pair, unpair, retry }}>{children}</Ctx.Provider>;
}
