// Runs INSIDE the webview. Bundled by scripts/build-terminal-engine.mjs into
// one inline HTML document; nothing here imports from the app.
import { FitAddon } from "@xterm/addon-fit";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { Terminal } from "@xterm/xterm";

import { concat, makeCoalescer } from "./coalesce";
import { decodeToEngine, encode, fromBase64, type FromEngine } from "./protocol";

declare global {
  interface Window {
    ReactNativeWebView?: { postMessage(s: string): void };
    __ainb?: { receive(raw: string): void };
  }
}

function post(msg: FromEngine) {
  window.ReactNativeWebView?.postMessage(encode(msg));
}

const term = new Terminal({
  allowProposedApi: true,
  cursorBlink: false,
  fontSize: 12,
  fontFamily: "Menlo, monospace",
  scrollback: 2000,
  theme: { background: "#191923", foreground: "#DCDCE6" },
  // OSC 8 links in pane output never navigate: the handler only reports the
  // activation to the host, which shows it and opens nothing either.
  linkHandler: { activate: (_event, uri) => post({ t: "link", uri }) },
});
term.loadAddon(new Unicode11Addon());
term.unicode.activeVersion = "11";
const fit = new FitAddon();
term.loadAddon(fit);

const root = document.getElementById("term")!;
term.open(root);

let readonly = true;
function setReadonly(on: boolean) {
  readonly = on;
  // No stdin while read only: the hidden textarea never takes focus, so a tap
  // does not raise the soft keyboard and nothing is buffered as input.
  term.options.disableStdin = on;
  term.textarea?.setAttribute("inputmode", on ? "none" : "text");
}
term.onData((data) => {
  if (!readonly) post({ t: "input", data });
});

let bytesWritten = 0;
let statsTimer: ReturnType<typeof setTimeout> | undefined;
const STATS_EVERY_MS = 250;
/** One stats message per quarter second at most, after the write has landed. */
function statsSoon() {
  if (statsTimer !== undefined) return;
  statsTimer = setTimeout(() => {
    statsTimer = undefined;
    post({ t: "stats", bytes: bytesWritten, cols: term.cols, rows: term.rows });
  }, STATS_EVERY_MS);
}
const writes = makeCoalescer<Uint8Array>(
  (batch) => {
    const data = concat(batch);
    bytesWritten += data.length;
    term.write(data, statsSoon);
  },
  (cb) => requestAnimationFrame(cb),
);

function refit() {
  fit.fit();
  post({ t: "fit", cols: term.cols, rows: term.rows });
}

window.__ainb = {
  receive(raw) {
    const msg = decodeToEngine(raw);
    if (!msg) return;
    switch (msg.t) {
      case "write":
        writes.push(fromBase64(msg.b64));
        break;
      case "clear":
        writes.drain();
        term.reset();
        break;
      case "fit":
        writes.drain();
        refit();
        break;
      case "readonly":
        setReadonly(msg.on);
        break;
    }
  },
};

window.addEventListener("resize", refit);
setReadonly(true);
refit();
post({ t: "ready" });
