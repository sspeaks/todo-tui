import type { Priority } from "@todo-tui/client/portable";
import { PRIORITY_PILLS } from "./presets";
import { Pill } from "./Pill";

/** +1 … +5 in one tap; the chosen one again clears it. */
export function QuickPriority({ value, onChange }: { value: Priority | undefined; onChange: (p: Priority | undefined) => void }) {
  return (
    <>
      {PRIORITY_PILLS.map((p) => (
        <Pill key={p.label} label={p.label} on={value === p.value} onPress={() => onChange(value === p.value ? undefined : p.value)} accessibilityLabel={`Priority ${p.label}`} />
      ))}
    </>
  );
}
