import { Redirect } from "expo-router";
import { ActivityIndicator, StyleSheet, View } from "react-native";
import { Home } from "../components/Home";
import { useSession } from "../session";
import { useTheme } from "../theme";

export default function Index() {
  const { session } = useSession();
  const t = useTheme();
  if (session.state === "unpaired") return <Redirect href="/pair" />;
  if (session.state === "loading")
    return (
      <View style={[styles.center, { backgroundColor: t.bg }]}>
        <ActivityIndicator color={t.accent} />
      </View>
    );
  return <Home />;
}

const styles = StyleSheet.create({ center: { flex: 1, alignItems: "center", justifyContent: "center" } });
