/**
 * The most capture text the daemon takes, in bytes of UTF-8 (design 2.3):
 * CaptureItem and ParseCapture fail with INVALID_ARGUMENT on "text" above it,
 * and UpdateItem on "item.title" for a longer (trimmed) title. Matches
 * `todo_core::MAX_CAPTURE_BYTES`.
 */
export const MAX_CAPTURE_BYTES = 64 * 1024;

// The filter limits (filter.proto), as `todo_core::filter` has them. A filter
// over one fails with INVALID_ARGUMENT: a group nested too deep on its own
// path, a total on "filter", too many sort keys on "sort".

/** How deep groups nest; a group in the Filter itself is 1 deep. */
export const MAX_GROUP_DEPTH = 4;
/** The most predicates a filter holds, in all its groups. */
export const MAX_PREDICATES = 128;
/** The most operands one predicate takes. */
export const MAX_PREDICATE_OPERANDS = 1000;
/** The most operands a filter holds, in all its predicates. */
export const MAX_OPERANDS = 4000;
/** The longest MATCHES text, in bytes of UTF-8. */
export const MAX_MATCHES_BYTES = 1024;
/** The most keys a sort holds, none twice. */
export const MAX_SORT_KEYS = 8;

/** ListItems' page size for page_size 0, as the store has it. */
export const DEFAULT_PAGE_SIZE = 100;
/** The largest page ListItems returns; a larger page_size is clamped to it. */
export const MAX_PAGE_SIZE = 1000;
