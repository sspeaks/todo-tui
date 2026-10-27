// The tabs' queries (Decision 9): a saved view's tab lists ListItems{view_id},
// the :search tab its own query over everything, and `/` narrows either
// with MATCHES on the title, the labels and the remote key.

import { type ClauseSchema, type FilterSchema, ItemField, type MessageInitShape, Operator, type ViewQuery } from "@todo-tui/client";
import type { TabId } from "./types.js";

const SEARCH = "search:";

/** The :search tab for `query`. */
export const searchTab = (query: string): TabId => `${SEARCH}${query}`;

/** The query of a :search tab; undefined for a saved view's tab. */
export const searchQuery = (tab: TabId): string | undefined => (tab.startsWith(SEARCH) ? tab.slice(SEARCH.length) : undefined);

/**
 * The text MATCHES should get, trimmed; undefined when it has nothing to
 * match on: blank (which MATCHES refuses) or without a letter or digit
 * (which FTS finds no words in).
 */
export function matchText(text: string): string | undefined {
  const trimmed = text.trim();
  return /[\p{L}\p{N}]/u.test(trimmed) ? trimmed : undefined;
}

type FilterInit = MessageInitShape<typeof FilterSchema>;
type ClauseInit = MessageInitShape<typeof ClauseSchema>;

const matches = (target: ItemField, text: string): ClauseInit => ({
  kind: { case: "predicate", value: { target, op: Operator.MATCHES, operands: [{ kind: { case: "text", value: text } }] } },
});

/** `/`: the title, the labels or the remote key match. */
const NARROWED = [ItemField.TITLE, ItemField.LABELS, ItemField.REMOTE_LINK_KEY];
/** :search: the description too. */
const SEARCHED = [...NARROWED, ItemField.DESCRIPTION];

/** `/`'s filter, ANDed onto the tab's own. */
export const narrowFilter = (text: string): FilterInit => ({ anyOf: NARROWED.map((f) => matches(f, text)) });

/**
 * :search's filter: every Item that matches, done, archived and deferred
 * ones too (not deleted ones); narrowed by `/` to those `narrow` matches.
 */
export const searchFilter = (text: string, narrow?: string): FilterInit => ({
  anyOf: SEARCHED.map((f) => matches(f, text)),
  ...(narrow !== undefined && { allOf: [{ kind: { case: "group", value: { anyOf: NARROWED.map((f) => matches(f, narrow)) } } }] }),
  includeArchived: true,
  includeDone: true,
  includeDeferred: true,
});

/** :search lists the most recently changed first. */
const SEARCH_SORT = [{ key: ItemField.UPDATED_AT, descending: true }];

/** The rows of `tab`, narrowed to those matching `narrow` when given. */
export function tabQuery(tab: TabId, narrow?: string): ViewQuery {
  const query = searchQuery(tab);
  if (query === undefined) return { viewId: tab, ...(narrow !== undefined && { filter: narrowFilter(narrow) }) };
  return { filter: searchFilter(query, narrow), sort: SEARCH_SORT };
}
