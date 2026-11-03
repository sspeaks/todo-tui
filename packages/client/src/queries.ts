// Thin helpers over the read RPCs behind the tab bar, the counts, the
// date prompts, the quick-add preview and the repeat prompt: ListViews,
// GetStatusCounts, ParseDate, ParseCapture and ParseRecurrence. The
// daemon does all the work; "today" is always the daemon's, in its zone.

import type { MessageInitShape } from "@bufbuild/protobuf";
import { type Timestamp, timestampFromDate } from "@bufbuild/protobuf/wkt";
import type { CallOptions, Client } from "@connectrpc/connect";
import type { DateSchema } from "./gen/google/type/date_pb.js";
import type { BuildMeetingLinkResponse, GetActivityReportResponse, GetStatusCountsResponse, ParseCaptureResponse, ParseDateResponse, ParseRecurrenceResponse } from "./gen/todo/v1/item_service_pb.js";
import type { ListViewsResponse, ParseQueryResponse, ViewService } from "./gen/todo/v1/view_service_pb.js";
import type { ItemsClient } from "./mutations.js";

export type ViewsClient = Client<typeof ViewService>;

export interface ReferenceOptions extends CallOptions {
  /** "Now", for today; defaults to the daemon's clock. */
  referenceTime?: Date | Timestamp;
}

function reference({ referenceTime, ...call }: ReferenceOptions) {
  const at = referenceTime instanceof Date ? timestampFromDate(referenceTime) : referenceTime;
  return [at === undefined ? {} : { referenceTime: at }, call] as const;
}

/** Every saved view, in tab order (position). */
export function listViews(client: ViewsClient, opts: CallOptions = {}): Promise<ListViewsResponse> {
  return client.listViews({}, opts);
}

/**
 * The inbox, due and overdue counts (`todo status`) and, with
 * `includeViews`, the tab count of each view whose show_count is set; all
 * read at one revision, with the today they used.
 */
export function getStatusCounts(
  client: ItemsClient,
  opts: ReferenceOptions & { includeViews?: boolean } = {},
): Promise<GetStatusCountsResponse> {
  const { includeViews, ...rest } = opts;
  const [fields, call] = reference(rest);
  return client.getStatusCounts({ ...fields, includeViews: includeViews ?? false }, call);
}

/**
 * The L48 / N48 report (design 6.6) over the [l48] window, or from
 * `since`, counted back from today ("thu", "-3d", "oct29"): what moved
 * and what is planned, as lines and as `text` to paste.
 */
export function getActivityReport(client: ItemsClient, opts: ReferenceOptions & { since?: string } = {}): Promise<GetActivityReportResponse> {
  const { since, ...rest } = opts;
  const [fields, call] = reference(rest);
  return client.getActivityReport({ ...fields, since: since ?? "" }, call);
}

/**
 * The Teams new-meeting link for `ids` (design 2.9), prefilled; nothing
 * is written. FAILED_PRECONDITION (MEETINGS_OFF) when [meetings] is off.
 */
export function buildMeetingLink(client: ItemsClient, ids: readonly string[], opts: CallOptions & { subject?: string } = {}): Promise<BuildMeetingLinkResponse> {
  const { subject, ...call } = opts;
  return client.buildMeetingLink({ ids: [...ids], ...(subject !== undefined && { subject }) }, call);
}

/**
 * Resolves a date expression with the capture date grammar (design 2.3),
 * e.g. "fri", "+3d", "eow", "nov2" or "2026-11-02", with an optional
 * leading "^". When it does not resolve, `date` is unset and `warning` says
 * why.
 */
export function parseDate(client: ItemsClient, text: string, opts: ReferenceOptions = {}): Promise<ParseDateResponse> {
  const [fields, call] = reference(opts);
  return client.parseDate({ ...fields, text }, call);
}

/**
 * Parses capture text as CaptureItem would (design 2.3) without writing
 * anything: the draft's title and fields, its tokens (offsets in code
 * points) and its warnings. Blank text gives an empty draft.
 */
export function parseCapture(client: ItemsClient, text: string, opts: ReferenceOptions = {}): Promise<ParseCaptureResponse> {
  const [fields, call] = reference(opts);
  return client.parseCapture({ ...fields, text }, call);
}

/**
 * Resolves a repeat rule with the capture rule grammar (design 2.3), e.g.
 * "daily", "mon,thu", "15th" or "2w", with an optional leading "@".
 * `weekly` and `monthly` take the anchor's weekday or day (the Item's due
 * date; defaults to today), and `first` is the first scheduled date on or
 * after it. When it does not parse, `rule` is unset and `warning` says why.
 */
export function parseRecurrence(
  client: ItemsClient,
  text: string,
  opts: ReferenceOptions & { anchor?: MessageInitShape<typeof DateSchema> } = {},
): Promise<ParseRecurrenceResponse> {
  const { anchor, ...rest } = opts;
  const [fields, call] = reference(rest);
  return client.parseRecurrence({ ...fields, text, ...(anchor !== undefined && { anchor }) }, call);
}

/**
 * Reads query text (design 4.8 "Query text") into a filter and sort, with
 * `count` the Items it lists. A problem is in `errors`, with its span in
 * code points; the call itself fails only for text over 4096 bytes.
 */
export function parseQuery(client: ViewsClient, text: string, opts: ReferenceOptions & { count?: boolean } = {}): Promise<ParseQueryResponse> {
  const { count, ...rest } = opts;
  const [fields, call] = reference(rest);
  return client.parseQuery({ ...fields, text, ...(count && { count }) }, call);
}
