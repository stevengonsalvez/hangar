import { createEffect, createMemo, createSignal, For, Index, onCleanup, Show, type JSX } from "solid-js";
import {
  geometry,
  groupOf,
  groups,
  moveTab,
  resize,
  splitGroup,
  type Divider,
  type Group,
  type GroupId,
  type Layout,
  type Rect,
  type SplitDirection,
} from "./layout.ts";
import { dropAt, dropTab, type Drop } from "./panes.ts";
import type { UiStatus } from "./status.ts";
import type { Tab } from "./tabs.ts";
import { TerminalTab } from "./terminal_tab.tsx";
import { setVisibleTerminals } from "./transport.ts";

interface Props {
  layout: Layout;
  /** The host's tabs, for each key's title, state and status. */
  tabs: readonly Tab[];
  /** Whether the terminals hold the work area: the panes stay mounted, and
   * hidden, under a page. */
  shown: boolean;
  title(tab: Tab): string;
  status(tab: Tab): UiStatus | null;
  onChoose(tab: Tab): void;
  onClose(tab: Tab): void;
  /** A split, a move or a resize: the layout it made. */
  onLayout(next: Layout): void;
  /** A pointer or the keyboard went into a group. */
  onFocusGroup(group: GroupId): void;
  /** "Close split pane": the window closes every tab of the group. */
  onCloseGroup(group: GroupId): void;
  /** Tab `key`'s terminal, shown while `visible` holds. Mounted once per
   * key, and only ever repositioned. */
  terminal(key: string, visible: () => boolean): JSX.Element;
  /** Drawn at the end of each group's strip, after its tabs, as Orca's "+"
   * sits there (`orca:src/renderer/src/components/tab-bar/tab-bar-surface.tsx:205-215`);
   * `shown` is that group's shown tab, which the "+" acts on. */
  stripEnd?(shown: () => Tab | undefined): JSX.Element;
}

/** How far a pointer travels on a tab before a press is a drag. */
const DRAG_PX = 4;

/** Orca's split directions, in its menu's order
 * (`orca:src/renderer/src/components/tab-bar/TabWorkspaceLayoutMenuSection.tsx:15`). */
const DIRECTIONS: readonly [SplitDirection, string][] = [
  ["right", "Right"],
  ["left", "Left"],
  ["down", "Down"],
  ["up", "Up"],
];

/** `rect` as the absolute position of a box in the panes, less `inset` at
 * its top when one is named (a strip's height, for the terminal under it). */
function place(rect: Rect | undefined, inset?: string): JSX.CSSProperties {
  if (rect === undefined) return { display: "none" };
  const [top, height] = [`${rect.y * 100}%`, `${rect.h * 100}%`];
  return {
    left: `${rect.x * 100}%`,
    top: inset === undefined ? top : `calc(${top} + ${inset})`,
    width: `${rect.w * 100}%`,
    height: inset === undefined ? height : `calc(${height} - ${inset})`,
  };
}

/** A seam's handle: a thin band centred on it, across the pair it divides. */
function seam(divider: Divider): JSX.CSSProperties {
  const { pair, at } = divider;
  return divider.axis === "row"
    ? { left: `${at * 100}%`, top: `${pair.y * 100}%`, height: `${pair.h * 100}%` }
    : { top: `${at * 100}%`, left: `${pair.x * 100}%`, width: `${pair.w * 100}%` };
}

type Menu =
  | { kind: "tab"; key: string; x: number; y: number }
  | { kind: "group"; group: GroupId; x: number; y: number };

/**
 * The terminal tab groups, side by side and stacked as the layout splits
 * them, as Orca draws its split groups
 * (`orca:src/renderer/src/components/tab-group/TabGroupPanel.tsx:254-261,291-321`):
 * each group has its own strip, since several show at once; a press in a
 * group focuses it; the focused group of several offers "Close split pane".
 * A tab moves by its menu ("Move Tab to Split", "Move to pane N") or by a
 * drag: onto a strip, a body's middle, or a body's edge to split there.
 * Terminals render in one layer beside the groups and are positioned over
 * their group, so a move never remounts one, as Orca keeps its live
 * surfaces at worktree level.
 */
export function Panes(props: Props) {
  let box!: HTMLDivElement;
  const all = createMemo(() => groups(props.layout));
  const ids = createMemo(
    () => all().map((group) => group.id),
    [],
    { equals: (a, b) => a.length === b.length && a.every((id, at) => id === b[at]) },
  );
  const byId = createMemo(() => new Map(all().map((group) => [group.id, group])));
  const byKey = createMemo(() => new Map(props.tabs.map((tab) => [tab.key, tab])));
  const keys = createMemo(
    () => props.tabs.map((tab) => tab.key),
    [],
    { equals: (a, b) => a.length === b.length && a.every((key, at) => key === b[at]) },
  );
  const shape = createMemo(() => geometry(props.layout));
  const split = () => ids().length > 1;

  // The terminals on screen, one per pane, told to the host on mount and on
  // every change (a split, a close, a tab shown in a pane, a page over the
  // panes): it keeps each attached past its cap, and lets each paste.
  const onScreen = createMemo(
    () => (props.shown ? all().flatMap((group) => (group.active === null ? [] : [group.active])) : []),
    [],
    { equals: (a, b) => a.length === b.length && a.every((key, at) => key === b[at]) },
  );
  createEffect(() => {
    const keys = onScreen();
    setVisibleTerminals(keys).then(
      (taken) => {
        // More panes than the host takes: it keeps the set it had.
        if (!taken) console.warn(`the host refused ${keys.length} terminals on screen; it keeps its last set`);
      },
      (error: unknown) => console.warn("the terminals on screen did not reach the host", error),
    );
  });

  // A seam being dragged: the layout it would make, drawn by the seams alone
  // until the drag ends, so the panes (and the tmux grids behind them) are
  // resized once, not on every pointer move.
  const [draft, setDraft] = createSignal<Layout | null>(null);
  const seams = createMemo(() => geometry(draft() ?? props.layout).dividers);
  // A tab being dragged, and where it would land.
  const [dragging, setDragging] = createSignal<{ key: string; drop: Drop | null } | null>(null);
  const [menu, setMenu] = createSignal<Menu | null>(null);

  // Window listeners a drag or an open menu holds, dropped when it ends or
  // when the panes go.
  let release: (() => void) | null = null;
  onCleanup(() => release?.());

  /** Listen on the window until `stop`; the one way every drag here tracks
   * a pointer that leaves the element it started on. */
  const track = (move: (event: PointerEvent) => void, stop: (event: PointerEvent) => void) => {
    release?.();
    const end = (event: PointerEvent) => {
      release?.();
      stop(event);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", end);
    window.addEventListener("pointercancel", end);
    release = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", end);
      window.removeEventListener("pointercancel", end);
      release = null;
    };
  };

  const startResize = (divider: Divider, down: PointerEvent) => {
    if (down.button !== 0) return;
    down.preventDefault();
    const layout = props.layout;
    const share = (event: PointerEvent) => {
      const window = box.getBoundingClientRect();
      const row = divider.axis === "row";
      const start = row ? window.left + divider.pair.x * window.width : window.top + divider.pair.y * window.height;
      const extent = row ? divider.pair.w * window.width : divider.pair.h * window.height;
      return ((row ? event.clientX : event.clientY) - start) / extent;
    };
    track(
      (event) => setDraft(resize(layout, divider.path, divider.index, share(event))),
      (event) => {
        setDraft(null);
        // A tab strip that landed mid-drag moved the layout on: the seam
        // measured at the press may no longer be there, so the drag is
        // dropped rather than laid over tabs that have since come and gone.
        if (event.type !== "pointerup" || props.layout !== layout) return;
        props.onLayout(resize(layout, divider.path, divider.index, share(event)));
      },
    );
  };

  const startDrag = (down: PointerEvent) => {
    const target = down.target as Element | null;
    const tab = target?.closest?.<HTMLElement>(".tab[data-key]");
    if (down.button !== 0 || tab === null || tab === undefined || target?.closest(".tab-close")) return;
    const key = tab.dataset.key!;
    const strip = tab.closest(".pane-strip")?.getBoundingClientRect().height;
    // The strip's own height; `--h-tabs` when the page has not laid it out.
    const stripPx = strip || 32;
    const dropFor = (event: PointerEvent) => {
      const over = (event.target as Element | null)?.closest?.<HTMLElement>(".tab[data-key]") ?? null;
      // Past a tab's middle is after it: before the tab that follows it, or
      // the strip's end.
      const rect = over?.getBoundingClientRect();
      const after = rect !== undefined && rect.width > 0 && event.clientX > rect.left + rect.width / 2;
      const next = after ? over?.nextElementSibling?.closest<HTMLElement>(".tab[data-key]") : over;
      const hovered = next?.dataset.key ?? null;
      return dropAt(props.layout, box.getBoundingClientRect(), { x: event.clientX, y: event.clientY }, stripPx, hovered);
    };
    track(
      (event) => {
        if (dragging() === null && Math.hypot(event.clientX - down.clientX, event.clientY - down.clientY) < DRAG_PX) return;
        setDragging({ key, drop: dropFor(event) });
      },
      (event) => {
        const drag = dragging();
        setDragging(null);
        if (drag === null || event.type !== "pointerup") return;
        // The press was a drag: the click it ends in is not a choice of tab.
        const swallow = (click: MouseEvent) => click.stopPropagation();
        window.addEventListener("click", swallow, { capture: true });
        setTimeout(() => window.removeEventListener("click", swallow, { capture: true }), 0);
        const drop = dropFor(event);
        if (drop !== null) props.onLayout(dropTab(props.layout, key, drop));
      },
    );
  };

  /** Where the dragged tab would land, as a box over the panes. */
  const preview = createMemo((): Rect | undefined => {
    const drop = dragging()?.drop;
    if (!drop) return undefined;
    const rect = shape().groups.get(drop.group);
    if (rect === undefined || drop.kind === "strip" || drop.zone === "center") return rect;
    const half = {
      right: { x: rect.x + rect.w / 2, w: rect.w / 2 },
      left: { w: rect.w / 2 },
      down: { y: rect.y + rect.h / 2, h: rect.h / 2 },
      up: { h: rect.h / 2 },
    };
    return { ...rect, ...half[drop.zone] };
  });

  /** Open `next`; Escape or a press outside it closes it, and gives the
   * keyboard back to where it was (a terminal, as a rule). */
  const openMenu = (next: Menu) => {
    release?.();
    const before = document.activeElement as HTMLElement | null;
    setMenu(next);
    const close = (event: Event) => {
      if (event.type === "keydown" && (event as KeyboardEvent).key !== "Escape") return;
      if (event.type === "pointerdown" && (event.target as Element | null)?.closest?.(".pane-menu")) return;
      release?.();
    };
    // After this press has finished, so it does not close what it opened.
    const armed = setTimeout(() => {
      window.addEventListener("pointerdown", close, true);
      window.addEventListener("keydown", close, true);
    }, 0);
    release = () => {
      clearTimeout(armed);
      window.removeEventListener("pointerdown", close, true);
      window.removeEventListener("keydown", close, true);
      const inMenu = document.activeElement?.closest?.(".pane-menu") != null;
      setMenu(null);
      release = null;
      if (inMenu && before?.isConnected) before.focus();
    };
  };
  const choose = (act: () => void) => {
    release?.();
    act();
  };

  const visible = (key: string) => {
    const id = groupOf(props.layout, key);
    return props.shown && id !== null && byId().get(id)?.active === key;
  };

  const strip = (group: () => Group | undefined, id: GroupId) => (
    <nav class="tabs pane-strip" aria-label={`Pane ${ids().indexOf(id) + 1} tabs`} onPointerDown={startDrag}>
      <For each={group()?.tabs ?? []}>
        {(key) => (
          <Show when={byKey().get(key)}>
            {(tab) => (
              <TerminalTab
                tab={tab()}
                title={props.title(tab())}
                active={group()?.active === key}
                status={props.status(tab())}
                onChoose={() => props.onChoose(tab())}
                onClose={() => props.onClose(tab())}
                onMenu={(event) => openMenu({ kind: "tab", key, x: event.clientX, y: event.clientY })}
              />
            )}
          </Show>
        )}
      </For>
      {props.stripEnd?.(() => byKey().get(group()?.active ?? ""))}
      <Show when={split() && props.layout.focused === id}>
        <button
          type="button"
          class="pane-actions"
          aria-label="Pane actions"
          onClick={(event) => openMenu({ kind: "group", group: id, x: event.clientX, y: event.clientY })}
        >
          …
        </button>
      </Show>
    </nav>
  );

  const tabMenu = (key: string) => {
    const from = groupOf(props.layout, key);
    const group = from === null ? undefined : byId().get(from);
    const tab = byKey().get(key);
    if (group === undefined || tab === undefined) return null;
    return (
      <>
        {/* A tab alone in its group has nowhere to split from: Orca leaves
            the entry out (`tab-move-to-pane-column.ts:26-28`). */}
        <Show when={group.tabs.length > 1}>
          <div class="pane-menu-heading">Move Tab to Split</div>
          <For each={DIRECTIONS}>
            {([direction, label]) => (
              <button
                type="button"
                role="menuitem"
                data-split={direction}
                onClick={() => choose(() => props.onLayout(splitGroup(props.layout, group.id, direction, key)))}
              >
                {label}
              </button>
            )}
          </For>
        </Show>
        <For each={ids().filter((id) => id !== group.id)}>
          {(id) => (
            <button
              type="button"
              role="menuitem"
              data-move={id}
              onClick={() => choose(() => props.onLayout(moveTab(props.layout, key, id)))}
            >
              Move to pane {ids().indexOf(id) + 1}
            </button>
          )}
        </For>
        <button type="button" role="menuitem" data-close="" onClick={() => choose(() => props.onClose(tab))}>
          Close
        </button>
      </>
    );
  };

  return (
    <div class="panes" ref={box} hidden={!props.shown} data-groups={ids().length}>
      <For each={ids()}>
        {(id) => {
          const group = () => byId().get(id);
          return (
            <section
              class="pane-group"
              data-group={id}
              data-focused={props.layout.focused === id ? "" : undefined}
              style={place(shape().groups.get(id))}
              onPointerDown={(event) => {
                // A press on a tab chooses that tab; anywhere else in the
                // group focuses the tab it shows.
                if (!(event.target as Element | null)?.closest?.(".tab")) props.onFocusGroup(id);
              }}
            >
              {strip(group, id)}
            </section>
          );
        }}
      </For>
      <For each={keys()}>
        {(key) => {
          const rect = () => {
            const id = groupOf(props.layout, key);
            return id === null ? undefined : shape().groups.get(id);
          };
          const focus = () => {
            const id = groupOf(props.layout, key);
            if (id !== null) props.onFocusGroup(id);
          };
          return (
            <div
              class="pane-slot"
              data-key={key}
              hidden={!visible(key)}
              style={place(rect(), "var(--h-tabs)")}
              onPointerDown={focus}
              onFocusIn={focus}
            >
              {props.terminal(key, () => visible(key))}
            </div>
          );
        }}
      </For>
      {/* By position, not by object: a drag makes new seams on every move,
          and a handle remounted under the pointer would drop its cursor. */}
      <Index each={seams()}>
        {(divider) => (
          <div
            class="pane-divider"
            role="separator"
            aria-orientation={divider().axis === "row" ? "vertical" : "horizontal"}
            data-axis={divider().axis}
            data-dragging={draft() !== null ? "" : undefined}
            style={seam(divider())}
            onPointerDown={(event) => startResize(divider(), event)}
          />
        )}
      </Index>
      <Show when={preview()}>{(rect) => <div class="pane-drop" style={place(rect())} />}</Show>
      <Show when={menu()}>
        {(open) => (
          <div
            class="pane-menu"
            role="menu"
            style={{ left: `${open().x}px`, top: `${open().y}px` }}
            // The keyboard goes to the first entry, as a menu's does.
            ref={(element) => queueMicrotask(() => element.querySelector<HTMLElement>("button")?.focus())}
          >
            {(() => {
              const shown = open();
              if (shown.kind === "tab") return tabMenu(shown.key);
              return (
                <button
                  type="button"
                  role="menuitem"
                  data-close-group=""
                  onClick={() => choose(() => props.onCloseGroup(shown.group))}
                >
                  Close split pane
                </button>
              );
            })()}
          </div>
        )}
      </Show>
    </div>
  );
}
