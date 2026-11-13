// The session: ready at once from the keychain's pairing, the hello
// retried in the background while the computer is out of reach, a refused
// token back to pairing, a daemon too old said so; the store shown from
// the kept list at launch, and from the last store's on return.
import AsyncStorage from "@react-native-async-storage/async-storage";
import { Code, ConnectError } from "@todo-tui/client/portable";
import { act, render, screen } from "@testing-library/react-native";
import { AppState, type AppStateStatus, Text } from "react-native";
import { saveCache } from "../cache";
import { SessionProvider, useSession } from "../session";

const mockHello = jest.fn();
/** Every store made, with the options it was made with. */
const mockStores: { opts: { cache?: unknown }; stopped: boolean }[] = [];
jest.mock("@todo-tui/client/portable", () => ({
  ...jest.requireActual<object>("@todo-tui/client/portable"),
  createRemoteTransport: () => ({}),
  createClients: () => ({ system: { getServerInfo: (...args: unknown[]) => mockHello(...args) }, items: {}, views: {}, watch: {} }),
  LiveStore: class {
    stopped = false;
    readonly opts: { cache?: unknown };
    constructor(mockOpts: { cache?: unknown }) {
      this.opts = mockOpts;
      mockStores.push(this);
    }
    start() {}
    async stop() {
      this.stopped = true;
    }
    /** What this store would carry to the next: itself, to tell which. */
    toCache() {
      return { from: mockStores.indexOf(this) };
    }
  },
}));
jest.mock("expo/fetch", () => ({ fetch: jest.fn() }));
const mockStored = new Map<string, string>();
jest.mock("expo-secure-store", () => ({
  AFTER_FIRST_UNLOCK: 0,
  getItemAsync: async (k: string) => mockStored.get(k) ?? null,
  setItemAsync: async (k: string, v: string) => void mockStored.set(k, v),
  deleteItemAsync: async (k: string) => void mockStored.delete(k),
}));

function State() {
  const { session } = useSession();
  return <Text testID="state">{session.state === "ready" ? `ready:${session.link.state}` : session.state}</Text>;
}

const unreachable = () => new ConnectError("fetch failed", Code.Unknown, undefined, undefined, new TypeError("fetch failed"));
const info = (apiMinor: number) => ({ daemonVersion: "0.1.0", apiMinor, capabilities: [] });

/** The AppState listeners, called as a change to `state` would. */
let appState: (state: AppStateStatus) => Promise<void>;

beforeEach(async () => {
  jest.useFakeTimers();
  mockHello.mockReset();
  mockStores.length = 0;
  mockStored.set("todo-tui.pairing", JSON.stringify({ url: "https://mac.ts.net", token: "todo_t" }));
  await AsyncStorage.clear();
  const listeners: ((s: AppStateStatus) => void)[] = [];
  jest.spyOn(AppState, "addEventListener").mockImplementation((_type, listener) => {
    listeners.push(listener as (s: AppStateStatus) => void);
    return { remove: () => listeners.splice(listeners.indexOf(listener as (s: AppStateStatus) => void), 1) };
  });
  appState = (state) => act(async () => listeners.forEach((l) => l(state)));
});
afterEach(() => {
  jest.useRealTimers();
  jest.restoreAllMocks();
});

const state = () => screen.getByTestId("state").props.children as string;
const advance = (ms: number) => act(async () => void jest.advanceTimersByTime(ms));
const start = () =>
  render(
    <SessionProvider>
      <State />
    </SessionProvider>,
  );

test("ready at once; the hello is retried 2 s then 4 s while unreachable, until it answers", async () => {
  mockHello.mockRejectedValueOnce(unreachable()).mockRejectedValueOnce(unreachable()).mockResolvedValueOnce(info(20));
  await start();
  // The list and adding work while the computer is out of reach.
  expect(state()).toBe("ready:unreachable");
  expect(mockHello).toHaveBeenCalledTimes(1);

  await advance(1_999);
  expect(mockHello).toHaveBeenCalledTimes(1);
  await advance(1);
  expect(mockHello).toHaveBeenCalledTimes(2);
  expect(state()).toBe("ready:unreachable");

  await advance(4_000);
  expect(mockHello).toHaveBeenCalledTimes(3);
  expect(state()).toBe("ready:ok");
  await advance(60_000);
  expect(mockHello).toHaveBeenCalledTimes(3);
});

test("a refused token goes back to pairing, forgets it, and stops", async () => {
  mockHello.mockRejectedValue(new ConnectError("nope", Code.Unauthenticated));
  await start();
  expect(state()).toBe("unpaired");
  expect(mockStored.has("todo-tui.pairing")).toBe(false);
  await advance(60_000);
  expect(mockHello).toHaveBeenCalledTimes(1);
});

test("a daemon older than the listener's API is too old", async () => {
  mockHello.mockResolvedValue(info(8));
  await start();
  expect(state()).toBe("ready:too-old");
});

test("without a pairing, it is unpaired", async () => {
  mockStored.clear();
  await start();
  expect(state()).toBe("unpaired");
  expect(mockHello).not.toHaveBeenCalled();
});

const EMPTY = { views: [], lists: [], items: [] };

test("launches showing the list kept for the paired computer", async () => {
  mockHello.mockResolvedValue(info(20));
  await saveCache("https://mac.ts.net", EMPTY);
  await start();
  expect(state()).toBe("ready:ok");
  expect(mockStores).toHaveLength(1);
  expect(mockStores[0]!.opts.cache).toEqual(EMPTY);
});

test("does not show a list kept for another computer", async () => {
  mockHello.mockResolvedValue(info(20));
  await saveCache("https://other.ts.net", EMPTY);
  await start();
  expect(state()).toBe("ready:ok");
  expect(mockStores[0]!.opts.cache).toBeUndefined();
});

test("back from the background, a new store starts from the last one's list; back from inactive, the store stays", async () => {
  mockHello.mockResolvedValue(info(20));
  await start();
  expect(mockStores).toHaveLength(1);

  // Control Center, say: the stream never stopped.
  await appState("inactive");
  await appState("active");
  expect(mockStores).toHaveLength(1);
  expect(mockStores[0]!.stopped).toBe(false);
  expect(mockHello).toHaveBeenCalledTimes(1);

  await appState("background");
  expect(mockStores[0]!.stopped).toBe(true);
  await appState("active");
  expect(mockStores).toHaveLength(2);
  expect(mockStores[1]!.opts.cache).toEqual({ from: 0 });
  expect(mockHello).toHaveBeenCalledTimes(2);
  expect(state()).toBe("ready:ok");
});

test("a refused token forgets the kept list too", async () => {
  mockHello.mockRejectedValue(new ConnectError("nope", Code.Unauthenticated));
  await saveCache("https://mac.ts.net", EMPTY);
  await start();
  expect(state()).toBe("unpaired");
  expect(await AsyncStorage.getItem("todo-tui.cache")).toBeNull();
});
