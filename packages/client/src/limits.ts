/**
 * The most capture text the daemon takes, in bytes of UTF-8 (design 2.3):
 * CaptureItem and ParseCapture fail with INVALID_ARGUMENT on "text" above it,
 * and UpdateItem on "item.title" for a longer (trimmed) title. Matches
 * `todo_core::MAX_CAPTURE_BYTES`.
 */
export const MAX_CAPTURE_BYTES = 64 * 1024;
