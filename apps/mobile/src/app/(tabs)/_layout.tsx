// The tab bar: the list, search and settings. Without a pairing, pairing
// comes first.
import { Ionicons } from "@expo/vector-icons";
import { Redirect, Tabs } from "expo-router";
import { ActivityIndicator, View } from "react-native";
import { useSession } from "../../session";
import { useTheme } from "../../theme";

export default function TabsLayout() {
  const t = useTheme();
  const { session } = useSession();
  if (session.state === "unpaired") return <Redirect href="/pair" />;
  if (session.state === "loading")
    return (
      <View style={{ flex: 1, alignItems: "center", justifyContent: "center", backgroundColor: t.bg }}>
        <ActivityIndicator color={t.accent} />
      </View>
    );
  return (
    <Tabs
      screenOptions={{
        tabBarActiveTintColor: t.accent,
        tabBarInactiveTintColor: t.dim,
        tabBarStyle: { backgroundColor: t.bg, borderTopColor: t.border },
        // The labels stay in the system font: the tab bar gives them a fixed height that JetBrains Mono's tall metrics overflow.
        headerStyle: { backgroundColor: t.bg },
        headerTintColor: t.text,
        headerTitleStyle: t.monoBold,
        // Navigation's default hairline is its light theme's, stark in dark mode; the tab strip under Home has its own.
        headerShadowVisible: false,
        sceneStyle: { backgroundColor: t.bg },
      }}
    >
      <Tabs.Screen
        name="index"
        options={{
          title: "todo",
          tabBarLabel: "List",
          tabBarIcon: ({ color, size, focused }) => <Ionicons name={focused ? "list" : "list-outline"} size={size} color={String(color)} />,
        }}
      />
      <Tabs.Screen
        name="search"
        options={{
          title: "Search",
          tabBarIcon: ({ color, size, focused }) => <Ionicons name={focused ? "search" : "search-outline"} size={size} color={String(color)} />,
        }}
      />
      <Tabs.Screen
        name="settings"
        options={{
          title: "Settings",
          tabBarIcon: ({ color, size, focused }) => <Ionicons name={focused ? "settings" : "settings-outline"} size={size} color={String(color)} />,
        }}
      />
    </Tabs>
  );
}
