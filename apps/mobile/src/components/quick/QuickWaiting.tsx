import { useState } from "react";
import { WaitingSheet } from "../pickers/WaitingSheet";
import { Pill } from "./Pill";

/** Who it waits on, one tap per name in use (again to clear); "Someone else" types another. */
export function QuickWaiting({
  value,
  known,
  onChange,
  limit,
}: {
  value: string | undefined;
  /** Most used first. */
  known: readonly string[];
  onChange: (name: string | undefined) => void;
  /** Show at most this many of the unchosen ones; the sheet has the rest. */
  limit?: number;
}) {
  const [typing, setTyping] = useState(false);
  const unchosen = known.filter((n) => n !== value);
  const shown = limit === undefined ? unchosen : unchosen.slice(0, limit);
  const names = value ? [value, ...shown] : shown;
  const more = unchosen.length > shown.length;
  return (
    <>
      {names.map((n) => (
        <Pill key={n} icon="person-outline" label={n} on={value === n} onPress={() => onChange(value === n ? undefined : n)} accessibilityLabel={`Waiting on ${n}`} />
      ))}
      <Pill icon="add" label={more ? "More…" : "Someone else"} onPress={() => setTyping(true)} accessibilityLabel="Waiting on someone else" />
      {typing ? <WaitingSheet visible value={value} known={known} onChange={onChange} onClose={() => setTyping(false)} /> : null}
    </>
  );
}
