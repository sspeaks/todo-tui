// The session: ready at once from the keychain's pairing, the hello
// retried in the background while the computer is out of reach, a refused
// token back to pairing, a daemon too old said so.
import { Code, ConnectError } from "@todo-tui/client/portable";
import { act, render, screen } from "@testing-library/react-native";
import { Text } from "react-native";
import { SessionProvider, useSession } from "../session";

const mockHello = jest.fn();
jest.mock("@todo-tui/client/portable", () => ({
  ...jest.requireActual<object>("@todo-tui/client/portable"),
  createRemoteTransport: () => ({}),
  createClients: () => ({ system: { getServerInfo: (...args: unknown[]) => mockHello(...args) }, items: {}, views: {}, watch: {} }),
  LiveStore: class {
    start() {}
    async stop() {}
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

beforeEach(() => {
  jest.useFakeTimers();
  mockHello.mockReset();
  mockStored.set("todo-tui.pairing", JSON.stringify({ url: "https://mac.ts.net", token: "todo_t" }));
});
afterEach(() => jest.useRealTimers());

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
