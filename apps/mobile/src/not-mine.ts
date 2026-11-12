// Someone else's Jira issue (design 5.9, ADR-0007): the daemon refuses a
// change of it with NOT_MINE until the person says yes. The phone asks with
// an alert, then sends the same request again (same id, so it applies once)
// with confirmNotMine.
import { errorReason, ulid } from "@todo-tui/client/portable";
import { Alert } from "react-native";

/** Asks whether to change someone else's issue in Jira; true on a yes. */
export type AskNotMine = (err: unknown) => Promise<boolean>;

/** The question as an alert: "Change it in Jira?" with the daemon's words. */
export const askWithAlert: AskNotMine = (err) =>
  new Promise((resolve) => {
    const why = err instanceof Error ? err.message.replace(/^\[[^\]]*\]\s*/, "") : "";
    Alert.alert("Someone else's Jira issue", `${why || "It is assigned to someone else."}\n\nChange it in Jira?`, [
      { text: "Cancel", style: "cancel", onPress: () => resolve(false) },
      { text: "Change it", style: "destructive", onPress: () => resolve(true) },
    ]);
  });

/**
 * Runs `call` once; on NOT_MINE asks, and on a yes runs it again with the
 * same request id and confirmNotMine. A no rethrows the refusal.
 */
export async function withNotMine<R>(
  call: (opts: { requestId: string; confirmNotMine?: boolean }) => Promise<R>,
  ask: AskNotMine = askWithAlert,
): Promise<R> {
  const requestId = ulid();
  try {
    return await call({ requestId });
  } catch (err) {
    if (errorReason(err) !== "NOT_MINE" || !(await ask(err))) throw err;
    return call({ requestId, confirmNotMine: true });
  }
}
