import { Redirect } from "expo-router";
import { ActivityIndicator, Pressable, StyleSheet, Text, View } from "react-native";
import { Home } from "../components/Home";
import { useSession } from "../session";
import { type Theme, useTheme } from "../theme";

export default function Index() {
  const { session, retry, unpair } = useSession();
  const t = useTheme();
  const s = styles(t);
  switch (session.state) {
    case "loading":
    case "connecting":
      return (
        <View style={s.center}>
          <ActivityIndicator color={t.accent} />
        </View>
      );
    case "unpaired":
      return <Redirect href="/pair" />;
    case "failed":
      return (
        <View style={s.center}>
          <Text style={s.message}>{session.message}</Text>
          <Pressable style={s.button} onPress={retry}>
            <Text style={s.buttonText}>Try again</Text>
          </Pressable>
        </View>
      );
    case "ready":
      return <Home onUnpair={unpair} />;
  }
}

const styles = (t: Theme) =>
  StyleSheet.create({
    center: { flex: 1, alignItems: "center", justifyContent: "center", padding: 24, gap: 16, backgroundColor: t.bg },
    message: { color: t.dim, textAlign: "center", fontSize: 15 },
    button: { backgroundColor: t.accent, borderRadius: 8, paddingHorizontal: 20, paddingVertical: 12 },
    buttonText: { color: t.onAccent, fontWeight: "600" },
  });
