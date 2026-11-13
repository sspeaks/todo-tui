import { Ionicons } from "@expo/vector-icons";
import { useConnection } from "@todo-tui/client/react";
import { Pressable, StyleSheet, Text, View } from "react-native";
import { describeError } from "../errors";
import { useSession } from "../session";
import { size, space, type Theme, useTheme } from "../theme";

/** Why the list may be stale: the computer out of reach, or its todo too old. Tap to retry now. */
export function Banner() {
  const t = useTheme();
  const s = styles(t);
  const conn = useConnection();
  const { session, retry } = useSession();
  const link = session.state === "ready" ? session.link : undefined;
  let text: string | undefined;
  const tooOld = link?.state === "too-old";
  if (tooOld) text = link.message;
  else if (link?.state === "unreachable" || (conn.state === "reconnecting" && conn.attempt > 1)) {
    const why = link?.state === "unreachable" ? link.message : conn.error ? describeError(conn.error) : "Reconnecting…";
    text = `${why} What you add is kept on this phone until then.`;
  }
  if (!text) return null;
  return (
    <Pressable onPress={retry} style={({ pressed }) => [s.banner, pressed && s.pressed]} accessibilityRole="button" accessibilityHint="Tries again now">
      <View style={s.edge} />
      <Ionicons name={tooOld ? "warning-outline" : "cloud-offline-outline"} size={18} color={t.warn} />
      <Text style={s.text}>{text}</Text>
    </Pressable>
  );
}

const styles = (t: Theme) =>
  StyleSheet.create({
    // A card marked on its edge, as a row's priority is.
    banner: {
      flexDirection: "row",
      alignItems: "center",
      gap: space.s,
      marginHorizontal: space.m,
      marginTop: space.s,
      paddingLeft: space.m + 3,
      paddingRight: space.m,
      paddingVertical: 10,
      borderRadius: 8,
      backgroundColor: t.surface,
      overflow: "hidden",
    },
    pressed: { opacity: 0.7 },
    edge: { position: "absolute", left: 0, top: 0, bottom: 0, width: 3, backgroundColor: t.warn },
    text: { flex: 1, color: t.warn, fontSize: size.meta, lineHeight: 18 },
  });
