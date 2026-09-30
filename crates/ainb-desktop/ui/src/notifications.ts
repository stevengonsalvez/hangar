// Whether the window raises an OS notification when a session needs you or is
// done: the person's toggle, on unless turned off.
//
// Kept like the theme pick (`theme/theme.ts`): localStorage is the one source,
// every read and write wrapped so a missing or throwing storage only loses the
// memory of it. The host sends the notifications, so it is told the toggle on
// every change and once at start, and keeps a copy for the next launch, when
// it may have a notification to send before this page has loaded
// (`notify.rs`).

import { createSignal, type Accessor } from "solid-js";
import type { ThemeStorage } from "./theme/theme.ts";

/** The localStorage key holding the toggle. */
export const NOTIFICATIONS_KEY = "ainb.notifications";

/** The stored toggle: on unless it says `off`, and on when unreadable. */
export function readNotifications(storage: ThemeStorage | undefined): boolean {
  try {
    return storage?.getItem(NOTIFICATIONS_KEY) !== "off";
  } catch {
    return true;
  }
}

/** Store the toggle; a storage that throws only loses the memory of it. */
export function writeNotifications(storage: ThemeStorage | undefined, enabled: boolean): void {
  try {
    storage?.setItem(NOTIFICATIONS_KEY, enabled ? "on" : "off");
  } catch {
    // Nothing to do: the host still has the toggle for this window's life.
  }
}

function safeStorage(): ThemeStorage | undefined {
  try {
    return window.localStorage;
  } catch {
    return undefined;
  }
}

/** The toggle a settings control reads and sets. */
export interface NotificationsControl {
  enabled: Accessor<boolean>;
  /** Turn notifications on or off: stored, and told to the host. */
  set(next: boolean): void;
}

/**
 * The stored toggle, told to the host now and on every change after, so the
 * host's copy converges on this page's even when it was never told.
 */
export function startNotifications(tellHost: (enabled: boolean) => void = () => {}): NotificationsControl {
  const [enabled, setEnabled] = createSignal(readNotifications(safeStorage()));
  tellHost(enabled());
  return {
    enabled,
    set(next) {
      setEnabled(next);
      writeNotifications(safeStorage(), next);
      tellHost(next);
    },
  };
}
