import type { Priority } from "@todo-tui/client/portable";
import { useTheme } from "../../theme";
import { PRIORITY_PILLS } from "./presets";
import { Pill } from "./Pill";

/** +1 … +5 in one tap, each with its colour; the chosen one again clears it. */
export function QuickPriority({ value, onChange }: { value: Priority | undefined; onChange: (p: Priority | undefined) => void }) {
  const t = useTheme();
  const dots = [t.p1, t.p2, t.p3, t.p4, t.p5];
  return (
    <>
      {PRIORITY_PILLS.map((p, i) => (
        <Pill
          key={p.label}
          label={p.label}
          dot={dots[i]!}
          on={value === p.value}
          onPress={() => onChange(value === p.value ? undefined : p.value)}
          accessibilityLabel={`Priority ${p.label}`}
        />
      ))}
    </>
  );
}
