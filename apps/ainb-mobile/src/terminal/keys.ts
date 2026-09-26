// The accessory bar above the soft keyboard: the keys a phone keyboard lacks.
// Each entry is the byte string xterm would send for that key.

export interface AccessoryKey {
  label: string;
  /** Bytes to send, or `ctrl` to toggle the control modifier. */
  data: string | "ctrl";
}

export const ACCESSORY_KEYS: AccessoryKey[] = [
  { label: "esc", data: "\x1b" },
  { label: "tab", data: "\t" },
  { label: "ctrl", data: "ctrl" },
  { label: "^C", data: "\x03" },
  { label: "↑", data: "\x1b[A" },
  { label: "↓", data: "\x1b[B" },
  { label: "←", data: "\x1b[D" },
  { label: "→", data: "\x1b[C" },
  { label: "⏎", data: "\r" },
];

/** Apply a pending ctrl modifier to one typed character: `c` becomes 0x03. */
export function withCtrl(data: string): string {
  if (data.length !== 1) return data;
  const code = data.toUpperCase().charCodeAt(0);
  if (code >= 0x40 && code <= 0x5f) return String.fromCharCode(code - 0x40);
  return data;
}
