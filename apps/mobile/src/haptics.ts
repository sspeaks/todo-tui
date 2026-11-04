// Small confirmations under the finger. Fire and forget: a device without
// haptics (or a failure) just stays silent.
import * as Haptics from "expo-haptics";

const quietly = (p: Promise<void>) => void p.catch(() => {});

/** A choice toggled. */
export const tick = () => quietly(Haptics.selectionAsync());
/** An item completed or reopened. */
export const tap = () => quietly(Haptics.impactAsync(Haptics.ImpactFeedbackStyle.Light));
/** Something added. */
export const success = () => quietly(Haptics.notificationAsync(Haptics.NotificationFeedbackType.Success));
