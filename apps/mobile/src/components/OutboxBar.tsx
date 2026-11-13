import { Ionicons } from "@expo/vector-icons";
import { StyleSheet, Text, View } from "react-native";
import { useOutbox } from "../outbox";
import { size, space, type Theme, useTheme } from "../theme";

/** What was added offline and waits to be sent. */
export function OutboxBar() {
  const t = useTheme();
  const s = styles(t);
  const { pending } = useOutbox();
  if (pending.length === 0) return null;
  return (
    <View style={s.bar} accessibilityRole="summary">
      <View style={s.edge} />
      <Ionicons name="cloud-upload-outline" size={18} color={t.accent} />
      <Text style={s.text} numberOfLines={2}>
        {pending.length === 1 ? "1 waiting to send" : `${pending.length} waiting to send`}: {pending.map((p) => p.title).join(", ")}
      </Text>
    </View>
  );
}

const styles = (t: Theme) =>
  StyleSheet.create({
    // As the banner: a card marked on its edge, in the accent (it is on its way, not wrong).
    bar: {
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
    edge: { position: "absolute", left: 0, top: 0, bottom: 0, width: 3, backgroundColor: t.accent },
    text: { flex: 1, color: t.dim, fontSize: size.meta, lineHeight: 18 },
  });
