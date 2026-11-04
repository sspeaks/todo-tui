import { useConnection } from "@todo-tui/client/react";
import { StyleSheet, Text } from "react-native";
import { describeError } from "../errors";
import { type Theme, useTheme } from "../theme";

/** Shown while the change stream is down: the list may be stale. */
export function Banner() {
  const s = styles(useTheme());
  const conn = useConnection();
  if (conn.state === "live" || conn.state === "closed") return null;
  if (conn.state === "connecting" && conn.attempt === 0) return null;
  return (
    <Text style={s.banner} numberOfLines={2}>
      Reconnecting{conn.attempt > 1 ? ` (${conn.attempt})` : ""}… {conn.error ? describeError(conn.error) : ""}
    </Text>
  );
}

const styles = (t: Theme) =>
  StyleSheet.create({
    banner: { backgroundColor: t.surface, color: t.warn, paddingHorizontal: 16, paddingVertical: 6, fontSize: 13 },
  });
