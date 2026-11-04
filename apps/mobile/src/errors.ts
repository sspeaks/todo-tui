import { Code, ConnectError, DaemonTooOldError, isUnreachable } from "@todo-tui/client/portable";

export const isUnauthenticated = (err: unknown): boolean => err instanceof ConnectError && err.code === Code.Unauthenticated;

/** One line for a failed call, in the words the person can act on. */
export function describeError(err: unknown): string {
  if (isUnreachable(err)) return "Can't reach your computer. Is it awake, on Tailscale, and running `tailscale serve`?";
  if (isUnauthenticated(err)) return "This phone isn't paired any more. Pair it again with `todo remote pair`.";
  if (err instanceof DaemonTooOldError) return `The todo on your computer is too old (${err.message}). Update it and run \`todo daemon stop\`.`;
  if (err instanceof ConnectError) return err.rawMessage || `Error ${Code[err.code]}`;
  return err instanceof Error ? err.message : String(err);
}
