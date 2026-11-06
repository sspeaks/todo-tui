import { useConnection } from "@todo-tui/client/react";
import { Pressable, StyleSheet, Text } from "react-native";
import { describeError } from "../errors";
import { useSession } from "../session";
import { type Theme, useTheme } from "../theme";

/** Why the list may be stale: the computer out of reach, or its todo too old. Tap to retry now. */
export function Banner() {
  const s = styles(useTheme());
  const conn = useConnection();
  const { session, retry } = useSession();
  const link = session.state === "ready" ? session.link : undefined;
  let text: string | undefined;
  if (link?.state === "too-old") text = link.message;
  else if (link?.state === "unreachable" || (conn.state === "reconnecting" && conn.attempt > 1)) {
    const why = link?.state === "unreachable" ? link.message : conn.error ? describeError(conn.error) : "Reconnecting…";
    text = `${why} What you add is kept on this phone until then.`;
  }
  if (!text) return null;
  return (
    <Pressable onPress={retry} accessibilityRole="button" accessibilityHint="Tries again now">
      <Text style={s.banner}>{text}</Text>
    </Pressable>
  );
}

const styles = (t: Theme) =>
  StyleSheet.create({
    banner: { backgroundColor: t.surface, color: t.warn, paddingHorizontal: 16, paddingVertical: 8, fontSize: 13 },
  });
