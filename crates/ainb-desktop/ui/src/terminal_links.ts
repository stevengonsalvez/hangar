import type { IBufferLine, ILink, ILinkProvider, Terminal } from "@xterm/xterm";
import { WebLinksAddon } from "@xterm/addon-web-links";

/**
 * The longest URL a pane links, Orca's bound (`terminal-http-link-limits.ts`)
 * and the host's (`links.rs`, `MAX_URL_BYTES`): past it, nothing is opened.
 * The page counts UTF-16 units and the host bytes, so a non-ASCII URL near
 * the bound can be linked here and still refused there, never the reverse.
 */
export const URL_MAX_LENGTH = 2048;

/** The vertical rules a TUI frames its panels with (Orca's layout frames). */
const FRAMES = "│┃║╎╏┆┇┊┋";

/**
 * The web links addon's own URL pattern, with a panel's vertical rule as a
 * stop too: `│https://a.b/c│` links `https://a.b/c`, not the rule after it.
 */
export const URL_PATTERN = new RegExp(
  `(https?|HTTPS?):[/]{2}[^\\s"'!*(){}|\\\\^<>\`${FRAMES}]*[^\\s"':,.!?{}|\\\\^~\\[\\]\`()<>${FRAMES}]`,
);

const SCHEME = /https?:\/\//i;
const SCHEME_START = /^https?:\/\//i;
const FRAGMENT = new RegExp(`^[^\\s"'!*(){}|\\\\^<>\`${FRAMES}]*`);
const FRAME = new RegExp(`[${FRAMES}|]`);
const NOT_LAYOUT = new RegExp(`[^\\s${FRAMES}|]`);
/** A row ending in one of these reads as cut mid-URL, however short it is. */
const CONTINUES = /[/?&=#%+:-]$/;
/** Orca's evidence that a framed URL really continued onto the rows below. */
const MIN_ROWS = 3;
const MIN_FILL = 0.8;

/** Where a pane gets its rows: an xterm buffer fits. */
export interface Rows {
  getLine(y: number): IBufferLine | undefined;
}

/** A row's text, and the cell each of its characters starts at (plus the end). */
interface Cells {
  text: string;
  columns: number[];
}

/** One row's share of a hard-wrapped URL: cells `start` up to, not including, `end`. */
export interface Span {
  y: number;
  start: number;
  end: number;
}

export interface WrappedUrl {
  url: string;
  spans: Span[];
}

function cells(line: IBufferLine): Cells {
  let text = "";
  const columns: number[] = [];
  let end = 0;
  for (let x = 0; x < line.length; x += 1) {
    const cell = line.getCell(x);
    // The second half of a wide character: its first half already counted it.
    if (!cell || cell.getWidth() === 0) continue;
    const chars = cell.getChars() || " ";
    text += chars;
    for (let i = 0; i < chars.length; i += 1) columns.push(x);
    end = x + cell.getWidth();
  }
  columns.push(end);
  return { text, columns };
}

/**
 * The URL that starts on row `startY` and runs down its panel onto row `y`,
 * or null. Each row carries its piece between the same left text and the
 * same right rule, and only rows that read as cut (filled to the rule, or
 * ending mid-URL) carry it on.
 */
function fromStart(read: (y: number) => Cells | undefined, startY: number, y: number): WrappedUrl | null {
  const start = read(startY);
  const scheme = start?.text.search(SCHEME) ?? -1;
  if (!start || scheme === -1 || !FRAME.test(start.text.slice(0, scheme))) return null;
  const prefix = start.text.slice(0, scheme);
  const schemeColumn = start.columns[scheme];
  let text = "";
  let rule: number | null = null;
  let carries = true;
  let startFilled = false;
  // Each row's piece as indexes into its text: from the scheme's to `to`.
  const pieces: { y: number; to: number }[] = [];
  for (let rowY = startY; carries && rowY < startY + URL_MAX_LENGTH; rowY += 1) {
    const row = read(rowY);
    if (!row || (rowY > startY && row.text.slice(0, scheme) !== prefix)) break;
    const piece = row.text.slice(scheme).match(FRAGMENT)?.[0] ?? "";
    // A second scheme is the next URL, not this one's tail.
    if (!piece || (rowY > startY && SCHEME_START.test(piece))) break;
    const end = scheme + piece.length;
    const suffix = row.text.slice(end);
    const ruleAt = suffix.search(FRAME);
    const ruleColumn = ruleAt === -1 ? undefined : row.columns[end + ruleAt];
    if (ruleColumn === undefined || (rule !== null && ruleColumn !== rule) || NOT_LAYOUT.test(suffix)) break;
    rule = ruleColumn;
    if (text.length + piece.length > URL_MAX_LENGTH) return null;
    pieces.push({ y: rowY, to: end });
    text += piece;
    const width = ruleColumn - schemeColumn;
    const fills = width > 0 && (row.columns[end] - schemeColumn) / width >= MIN_FILL;
    if (rowY === startY) startFilled = fills;
    carries = CONTINUES.test(piece) || fills;
  }
  if (pieces.length > 1 && (pieces.length < MIN_ROWS || !startFilled)) {
    // A short URL may end in `/`; one framed word under it is not its tail.
    pieces.splice(1);
    text = text.slice(0, pieces[0].to - scheme);
  }
  const last = pieces.at(-1);
  if (!last || last.y < y) return null;
  // The pattern trims what a sentence puts after a URL, as the addon's does.
  const url = text.match(URL_PATTERN);
  if (!url || url.index !== 0) return null;
  // Each row's span, cut back to the characters the URL kept.
  const spans: Span[] = [];
  let left = url[0].length;
  for (const piece of pieces) {
    if (left <= 0) break;
    const { columns } = read(piece.y)!;
    const to = Math.min(piece.to, scheme + left);
    spans.push({ y: piece.y, start: columns[scheme], end: columns[to] });
    left -= to - scheme;
  }
  return { url: url[0], spans };
}

/**
 * The URL a TUI printed across several framed rows that row `y` (0-based) is
 * part of, as Orca's `buildHardWrappedHttpLogicalLineCandidates` finds it: a
 * panel wraps its text itself, with its rules on either side, so the terminal
 * sees separate rows where a soft wrap would have joined them. One-row URLs
 * are the web links addon's.
 */
export function hardWrappedUrl(buffer: Rows, y: number): WrappedUrl | null {
  const here = buffer.getLine(y);
  // Only a framed row can hold a piece of one: most rows skip the scan up.
  if (!here || !FRAME.test(here.translateToString(false))) return null;
  const seen = new Map<number, Cells | undefined>();
  const read = (rowY: number) => {
    if (!seen.has(rowY)) {
      const line = buffer.getLine(rowY);
      seen.set(rowY, line && cells(line));
    }
    return seen.get(rowY);
  };
  let best: WrappedUrl | null = null;
  for (let startY = y; startY >= Math.max(0, y - URL_MAX_LENGTH + 1); startY -= 1) {
    const text = buffer.getLine(startY)?.translateToString(false) ?? "";
    // Every row of a framed URL is framed: the first row up that is not (a
    // panel's top edge, a prompt) ends the scan.
    if (!FRAME.test(text)) break;
    // The scheme is looked for in the plain text first: building a row's
    // cells costs a read of each, and most rows above have no URL at all.
    if (!SCHEME.test(text)) continue;
    const found = fromStart(read, startY, y);
    if (found && found.spans.length > 1 && (!best || found.spans.length > best.spans.length)) best = found;
  }
  return best;
}

/** Whether a click opens the link under it: Cmd+click on macOS, Ctrl+click elsewhere, as Orca's panes do. */
export function linkClick(event: Pick<MouseEvent, "button" | "altKey" | "metaKey" | "ctrlKey">, mac: boolean): boolean {
  return event.button === 0 && !event.altKey && (mac ? event.metaKey : event.ctrlKey);
}

/**
 * Link every URL `term` shows: one-row and soft-wrapped ones through the web
 * links addon, framed hard-wrapped ones through `hardWrappedUrl`, and OSC 8
 * hyperlinks. Hovering one names its target on `pane`; a link click
 * (`linkClick`) on it hands the URL to `open`. Nothing here opens one.
 */
export function loadTerminalLinks(term: Terminal, pane: HTMLElement, mac: boolean, open: (url: string) => void): void {
  // The URL under the pointer, as xterm reports it. The tooltip shows where a
  // link goes, which an OSC 8 link's text need not say.
  let under: string | null = null;
  const hover = (_event: MouseEvent, url: string) => {
    under = url;
    pane.title = url;
  };
  const leave = () => {
    under = null;
    pane.removeAttribute("title");
  };
  // A link click is taken before xterm sees it, press and release, so it is
  // not also sent to the pane's program: the pane is a tmux client with
  // tmux's mouse on, which would take it as a click of its own.
  const take = (event: MouseEvent) => {
    if (!under || !linkClick(event, mac) || !term.element?.contains(event.target as Node)) return;
    event.preventDefault();
    event.stopPropagation();
    if (event.type === "mouseup") open(under);
  };
  pane.addEventListener("mousedown", take, true);
  pane.addEventListener("mouseup", take, true);
  // xterm's own activation: a link click never reaches it, and a bare click
  // is the pane's.
  const activate = () => {};
  const wrapped: ILinkProvider = {
    provideLinks(line, done) {
      const found = hardWrappedUrl(term.buffer.active, line - 1);
      const links: ILink[] | undefined = found?.spans
        .filter((span) => span.y === line - 1)
        .map((span) => ({
          range: { start: { x: span.start + 1, y: line }, end: { x: span.end, y: line } },
          text: found.url,
          activate,
          hover,
          leave,
        }));
      done(links?.length ? links : undefined);
    },
  };
  // Registered before the addon: the first provider to find a link under the
  // pointer wins, so a framed URL opens whole rather than as its first row.
  term.registerLinkProvider(wrapped);
  term.loadAddon(new WebLinksAddon(activate, { urlRegex: URL_PATTERN, hover, leave }));
  // OSC 8 links the same way, not through xterm's own `window.open`.
  term.options.linkHandler = { activate, hover, leave, allowNonHttpProtocols: false };
}
