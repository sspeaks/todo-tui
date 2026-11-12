import { Code, ConnectError, ErrorInfoSchema } from "@todo-tui/client/portable";
import { withNotMine } from "../not-mine";

const notMine = () =>
  new ConnectError("PLAT-4 is assigned to Alex", Code.FailedPrecondition, undefined, [
    { desc: ErrorInfoSchema, value: { reason: "NOT_MINE", domain: "todo.v1" } },
  ]);

test("a yes sends the same request again, confirmed", async () => {
  const calls: { requestId: string; confirmNotMine?: boolean }[] = [];
  const out = await withNotMine(
    async (opts) => {
      calls.push(opts);
      if (!opts.confirmNotMine) throw notMine();
      return "done";
    },
    async () => true,
  );
  expect(out).toBe("done");
  expect(calls).toHaveLength(2);
  expect(calls[1]).toEqual({ requestId: calls[0]!.requestId, confirmNotMine: true });
});

test("a no keeps the refusal, and other errors are not asked about", async () => {
  const ask = jest.fn(async () => false);
  await expect(withNotMine(async () => Promise.reject(notMine()), ask)).rejects.toThrow("assigned to Alex");
  expect(ask).toHaveBeenCalledTimes(1);
  ask.mockClear();
  await expect(withNotMine(async () => Promise.reject(new Error("offline")), ask)).rejects.toThrow("offline");
  expect(ask).not.toHaveBeenCalled();
});
