// The session: pairing from the keychain, automatic retries while the
// computer is unreachable, and back to pairing when the token is refused.
import { Code, ConnectError } from "@todo-tui/client/portable";
import { act, render, screen } from "@testing-library/react-native";
import { Text } from "react-native";
import { SessionProvider, useSession } from "../session";

const mockConnect = jest.fn();
jest.mock("@todo-tui/client/portable", () => ({
  ...jest.requireActual<object>("@todo-tui/client/portable"),
  connectRemote: (...args: unknown[]) => mockConnect(...args),
  createRemoteLiveStore: () => ({ stop: async () => {} }),
}));
jest.mock("expo/fetch", () => ({ fetch: jest.fn() }));
const mockStored = new Map<string, string>([["todo-tui.pairing", JSON.stringify({ url: "https://mac.ts.net", token: "todo_t" })]]);
jest.mock("expo-secure-store", () => ({
  AFTER_FIRST_UNLOCK: 0,
  getItemAsync: async (k: string) => mockStored.get(k) ?? null,
  setItemAsync: async (k: string, v: string) => void mockStored.set(k, v),
  deleteItemAsync: async (k: string) => void mockStored.delete(k),
}));

function State() {
  const { session } = useSession();
  return <Text testID="state">{session.state === "failed" ? `failed:${session.retrying}` : session.state}</Text>;
}

const unreachable = () => new ConnectError("fetch failed", Code.Unknown, undefined, undefined, new TypeError("fetch failed"));

beforeEach(() => {
  jest.useFakeTimers();
  mockConnect.mockReset();
});
afterEach(() => jest.useRealTimers());

const state = () => screen.getByTestId("state").props.children as string;
const advance = (ms: number) => act(async () => void jest.advanceTimersByTime(ms));

test("an unreachable computer is retried on its own, 2 s then 4 s, until it answers", async () => {
  mockConnect.mockRejectedValueOnce(unreachable()).mockRejectedValueOnce(unreachable()).mockResolvedValueOnce({ serverInfo: {} });
  await render(
    <SessionProvider>
      <State />
    </SessionProvider>,
  );
  expect(state()).toBe("failed:true");
  expect(mockConnect).toHaveBeenCalledTimes(1);

  await advance(1_999);
  expect(mockConnect).toHaveBeenCalledTimes(1);
  await advance(1);
  expect(mockConnect).toHaveBeenCalledTimes(2);
  // A retry keeps the failure on screen, not a spinner.
  expect(state()).toBe("failed:true");

  await advance(4_000);
  expect(mockConnect).toHaveBeenCalledTimes(3);
  expect(state()).toBe("ready");
  // Nothing more is scheduled once connected.
  await advance(60_000);
  expect(mockConnect).toHaveBeenCalledTimes(3);
});

test("a refused token goes back to pairing, forgets it, and stops retrying", async () => {
  mockConnect.mockRejectedValue(new ConnectError("nope", Code.Unauthenticated));
  await render(
    <SessionProvider>
      <State />
    </SessionProvider>,
  );
  expect(state()).toBe("unpaired");
  expect(mockStored.has("todo-tui.pairing")).toBe(false);
  await advance(60_000);
  expect(mockConnect).toHaveBeenCalledTimes(1);
});
