import { useState } from "react";
import { WaitingSheet } from "../pickers/WaitingSheet";
import { Pill } from "./Pill";

/** Who it waits on, one tap per name in use (again to clear); "+ ⧗" types another. */
export function QuickWaiting({ value, known, onChange }: { value: string | undefined; known: readonly string[]; onChange: (name: string | undefined) => void }) {
  const [typing, setTyping] = useState(false);
  const names = value && !known.includes(value) ? [value, ...known] : known;
  return (
    <>
      {names.map((n) => (
        <Pill key={n} label={`⧗ ${n}`} on={value === n} onPress={() => onChange(value === n ? undefined : n)} accessibilityLabel={`Waiting on ${n}`} />
      ))}
      <Pill label="+ ⧗" onPress={() => setTyping(true)} accessibilityLabel="Waiting on someone else" />
      {typing ? <WaitingSheet visible value={value} known={known} onChange={onChange} onClose={() => setTyping(false)} /> : null}
    </>
  );
}
