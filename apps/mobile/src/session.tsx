// The connection to the paired daemon: read the pairing, connect, keep a
// LiveStore while the app is in the foreground. A store is dropped in the
// background (iOS suspends its stream anyway) and made afresh on return.
import {
  type LiveStore,
  type Pairing,
  type RemoteConnection,
  connectRemote,
  createRemoteLiveStore,
} from "@todo-tui/client/portable";
import { fetch as expoFetch } from "expo/fetch";
import { createContext, type ReactNode, useCallback, useContext, useEffect, useRef, useState } from "react";
import { AppState } from "react-native";
import { describeError, isUnauthenticated } from "./errors";
import { clearPairing, loadPairing, savePairing } from "./pairing";

export type Session =
  | { state: "loading" }
  | { state: "unpaired"; message?: string }
  | { state: "connecting"; pairing: Pairing }
  /** Unreachable or refused for now: retried on its own while the app is in front. */
  | { state: "failed"; pairing: Pairing; message: string; retrying: boolean }
  | { state: "ready"; pairing: Pairing; conn: RemoteConnection; store: LiveStore };

interface SessionApi {
  session: Session;
  /** Checks the pairing against the daemon, then keeps it. Rejects with a message to show. */
  pair(p: Pairing): Promise<void>;
  unpair(message?: string): Promise<void>;
  retry(): void;
}

const Ctx = createContext<SessionApi | null>(null);

export function useSession(): SessionApi {
  const api = useContext(Ctx);
  if (!api) throw new Error("useSession outside <SessionProvider>");
  return api;
}

const connect = (p: Pairing) =>
  connectRemote({ baseUrl: p.url, token: p.token, fetch: expoFetch as unknown as typeof globalThis.fetch, timeoutMs: 15_000 });

export function SessionProvider({ children }: { children: ReactNode }) {
  const [session, setState] = useState<Session>({ state: "loading" });
  /** The session as last set, for callbacks that outlive a render. */
  const current = useRef<Session>(session);
  const setSession = useCallback((s: Session) => {
    current.current = s;
    setState(s);
  }, []);
  /** Bumped by every connect, so a slow one that lost the race is dropped. */
  const attempt = useRef(0);
  /** The next automatic retry, and how many failed in a row. */
  const retryTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const failures = useRef(0);
  const clearRetry = useCallback(() => {
    if (retryTimer.current !== undefined) clearTimeout(retryTimer.current);
    retryTimer.current = undefined;
  }, []);

  /**
   * Connects. A failure that is not a refused token is retried on its own,
   * 2 s after the first and doubling to 30 s, while the app is in front;
   * `quiet` (a retry) keeps the failure on screen instead of a spinner.
   */
  const open = useCallback(
    async function open(pairing: Pairing, quiet = false): Promise<void> {
      clearRetry();
      const mine = ++attempt.current;
      if (!quiet) setSession({ state: "connecting", pairing });
      try {
        const conn = await connect(pairing);
        if (mine !== attempt.current) return;
        failures.current = 0;
        setSession({ state: "ready", pairing, conn, store: createRemoteLiveStore(conn) });
      } catch (err) {
        if (mine !== attempt.current) return;
        if (isUnauthenticated(err)) {
          failures.current = 0;
          await clearPairing();
          setSession({ state: "unpaired", message: describeError(err) });
          return;
        }
        const delay = Math.min(30_000, 2_000 * 2 ** failures.current++);
        retryTimer.current = setTimeout(() => {
          if (AppState.currentState !== "background") void open(pairing, true);
        }, delay);
        setSession({ state: "failed", pairing, message: describeError(err), retrying: true });
      }
    },
    [setSession, clearRetry],
  );

  const close = useCallback(() => {
    attempt.current++;
    clearRetry();
    const s = current.current;
    if (s.state === "ready") void s.store.stop();
  }, [clearRetry]);

  useEffect(() => {
    void loadPairing().then((p) => (p ? open(p) : setSession({ state: "unpaired" })));
    const sub = AppState.addEventListener("change", (state) => {
      const s = current.current;
      if (state === "active" && s.state === "ready") {
        // A fresh store on the same connection: the session stays ready, so
        // the screens (and where you were) stay mounted.
        void s.store.stop();
        setSession({ ...s, store: createRemoteLiveStore(s.conn) });
      } else if (state === "active" && s.state === "failed") {
        failures.current = 0;
        void open(s.pairing, true);
      } else if (state === "background") {
        // Nothing runs in the background: the retries wait for the app.
        clearRetry();
        if (s.state === "ready") void s.store.stop();
      }
    });
    return () => {
      sub.remove();
      close();
    };
  }, [open, close, setSession, clearRetry]);

  const pair = useCallback(
    async (p: Pairing) => {
      try {
        await connect(p);
      } catch (err) {
        throw new Error(describeError(err));
      }
      await savePairing(p);
      close();
      await open(p);
    },
    [open, close],
  );

  const unpair = useCallback(
    async (message?: string) => {
      close();
      await clearPairing();
      setSession(message === undefined ? { state: "unpaired" } : { state: "unpaired", message });
    },
    [close, setSession],
  );

  const retry = useCallback(() => {
    const s = current.current;
    if (s.state === "failed" || s.state === "ready") {
      close();
      failures.current = 0;
      void open(s.pairing, s.state === "failed");
    }
  }, [open, close]);

  return <Ctx.Provider value={{ session, pair, unpair, retry }}>{children}</Ctx.Provider>;
}
