// What the status bar's usage segment draws (Orca O15): section 21, the same
// body the stats tab draws, as a row of per-provider chips.
//
//   usage section ──usageSegment──▶ state, chips, "+N", tooltip
//
// Orca's ProviderSegment shows each provider's tightest rate-limit window as a
// percentage. The daemon's summary carries no limit and no reset time, only the
// trailing thirty days' tokens and cost, so a chip shows that cost (its tokens
// when unpriced) and there is no "near limit" tone to draw. The states are
// Orca's: loading, unavailable ("--"), a failed read with nothing held, and
// held numbers with a stale mark. Wording on where the read stands is
// `statsView`'s, so the tooltip and the stats tab never disagree.

import type { UsageBucketFrame, UsageView } from "../../../ainb-app/bindings/AppState";
import { formatCost, statsView, tokens } from "./stats.ts";

/** Where the segment's read stands. */
export type UsageSegmentState = "loading" | "unavailable" | "failed" | "empty" | "ready";

/** One provider's chip. */
export interface UsageChip {
  provider: string;
  figure: string;
}

export interface UsageSegment {
  state: UsageSegmentState;
  /** Drawn chips, at most `MAX_CHIPS`, in the daemon's order. */
  chips: UsageChip[];
  /** Providers past the cap or cut by the frame: the "+N" chip, 0 for none. */
  more: number;
  /** The "+N" chip's tooltip, naming what it stands for. */
  moreTitle: string;
  /** The numbers drawn are not the daemon's latest complete word. */
  stale: boolean;
  /** The segment's tooltip. */
  title: string;
}

/** Chips a 24px bar holds before the rest fold into "+N". */
export const MAX_CHIPS = 3;

const COMPACT = new Intl.NumberFormat("en-US", { notation: "compact", maximumFractionDigits: 1 });

/** A token count short enough for the status bar: 950, 12.3K, 1.2M. */
export function compactTokens(count: number): string {
  return COMPACT.format(count);
}

/** A provider's figure: its cost, or its tokens when a call had no rate. */
export function chipFigure(bucket: UsageBucketFrame): string {
  return bucket.cost_usd === null ? `${compactTokens(tokens(bucket))} tok` : formatCost(bucket.cost_usd);
}

const READY_TITLE = "Usage over the trailing 30 days";

/**
 * Section 21 as the segment draws it. `stale` is whether the host withheld the
 * section since the body held here was framed, as for the stats tab.
 */
export function usageSegment(usage: UsageView | undefined, stale = false): UsageSegment {
  const stats = statsView(usage, stale);
  const title = stats.status ?? READY_TITLE;
  const none = { chips: [], more: 0, moreTitle: "", stale: false, title };
  const summary = usage?.summary ?? null;
  // Scanning has counted nothing yet, whatever the reply carries.
  if (summary === null || summary.state === "scanning") {
    const state = stats.state === "absent" ? "unavailable" : stats.state === "failed" ? "failed" : "loading";
    return { ...none, state };
  }
  const providers = summary.providers.map((entry) => ({ provider: entry.name, figure: chipFigure(entry.bucket) }));
  if (providers.length === 0) return { ...none, state: summary.state === "unavailable" ? "unavailable" : "empty" };
  const folded = providers.slice(MAX_CHIPS).map((chip) => `${chip.provider} ${chip.figure}`);
  if (summary.providers_cut > 0) folded.push(`${summary.providers_cut} not sent`);
  return {
    state: "ready",
    chips: providers.slice(0, MAX_CHIPS),
    more: providers.length - Math.min(providers.length, MAX_CHIPS) + summary.providers_cut,
    moreTitle: folded.length === 0 ? "" : `Also: ${folded.join(", ")}`,
    stale: summary.state !== "ready" || usage?.failure != null || stale,
    title,
  };
}
