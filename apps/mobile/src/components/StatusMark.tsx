// The TUI's status glyphs (○ ◐ ● ⊘) drawn as icons: an open ring, a half
// ring in progress, a filled check when done, a slash when cancelled. It
// pops when it turns done.
import { Ionicons } from "@expo/vector-icons";
import { Status } from "@todo-tui/client/portable";
import { useEffect, useRef, useState } from "react";
import { Animated } from "react-native";
import { type Theme, useTheme } from "../theme";

type Icon = keyof typeof Ionicons.glyphMap;

function look(status: Status, t: Theme): { icon: Icon; color: string } {
  switch (status) {
    case Status.IN_PROGRESS:
      return { icon: "contrast", color: t.accent };
    case Status.DONE:
      return { icon: "checkmark-circle", color: t.accent };
    case Status.CANCELLED:
      return { icon: "ban-outline", color: t.dim };
    default:
      return { icon: "ellipse-outline", color: t.dim };
  }
}

export function StatusMark({ status, size = 24 }: { status: Status; size?: number }) {
  const t = useTheme();
  const [scale] = useState(() => new Animated.Value(1));
  const was = useRef(status);
  useEffect(() => {
    if (status === Status.DONE && was.current !== Status.DONE) {
      scale.setValue(0.6);
      Animated.spring(scale, { toValue: 1, friction: 4, tension: 160, useNativeDriver: true }).start();
    }
    was.current = status;
  }, [status, scale]);
  const { icon, color } = look(status, t);
  return (
    <Animated.View style={{ transform: [{ scale }] }} testID={`status-${Status[status]?.toLowerCase() ?? "unknown"}`}>
      <Ionicons name={icon} size={size} color={color} />
    </Animated.View>
  );
}
