import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Pressable, StyleSheet, Text, View } from "react-native";
import { WebView, type WebViewMessageEvent } from "react-native-webview";

import { colors } from "../theme";
import { TERMINAL_HTML } from "./engine/bundle.generated";
import { decodeFromEngine, injection, toBase64, type ToEngine } from "./engine/protocol";
import { ACCESSORY_KEYS, withCtrl } from "./keys";

/**
 * The inline document's origin. A reserved `.invalid` name (RFC 2606) that no
 * resolver answers, given to the webview as `baseUrl` so the page has a REAL
 * origin: an inline page without one is opaque, and a modern Android WebView
 * then reports every message as coming from `null` (react-native-webview
 * delivers `postMessage` through `WebMessageListener` and puts
 * `sourceOrigin.toString()` in `nativeEvent.url`), which the origin check
 * below must drop. The CSP still forbids every fetch from this origin.
 */
export const ENGINE_ORIGIN = "https://terminal.ainb.invalid";
/** The document URL: what iOS (`frameInfo.request.URL`) and an older Android (`getUrl()`) report. */
export const ENGINE_URL = `${ENGINE_ORIGIN}/`;
/** Exactly the two spellings the platforms report for this one page, nothing wider. */
const ENGINE_SOURCES: ReadonlySet<string> = new Set([ENGINE_ORIGIN, ENGINE_URL]);

/**
 * Terminal output is attacker-influenced (any program in the pane can print
 * an OSC 8 link), so the webview may never leave the inline document, open
 * a window, or read files. The engine itself is built with the link handler
 * disabled and a CSP that allows only its inline script and style.
 */
export const LOCKDOWN = {
  originWhitelist: [ENGINE_ORIGIN],
  onShouldStartLoadWithRequest: (req: { url: string }) => req.url === ENGINE_URL,
  setSupportMultipleWindows: false,
  javaScriptCanOpenWindowsAutomatically: false,
  allowFileAccess: false,
  allowFileAccessFromFileURLs: false,
  allowUniversalAccessFromFileURLs: false,
  mixedContentMode: "never" as const,
  incognito: true,
  cacheEnabled: false,
};

/** Only the origin of a rejected sender is shown, never its path or query. */
export function originOf(url: string): string {
  const m = /^([a-z][a-z0-9+.-]*:\/\/[^/?#]+)/i.exec(url);
  return m?.[1] ?? url.split(/[/?#]/)[0] ?? url;
}

export interface TerminalSink {
  write(bytes: Uint8Array): void;
  clear(): void;
}

export interface TerminalViewProps {
  /**
   * Called on mount with the sink to feed bytes into. Writes made before the
   * engine is up queue and inject once it reports `ready`, so a snapshot that
   * arrives while the webview is still loading is never lost.
   */
  onSink(sink: TerminalSink): void;
  /** Keystrokes from the soft keyboard or the accessory bar; absent = read only. */
  onInput?(data: string): void;
  onFit?(cols: number, rows: number): void;
  /** The engine reports a link tap it refused to open. */
  onLink?(uri: string): void;
  /** Engine lifecycle for the status row: loading, ready, or a dropped message with its origin. */
  onEngine?(state: string): void;
  testID?: string;
}

/**
 * xterm.js inside a webview. Bytes queue until the engine says `ready`, then
 * cross the bridge as base64; the engine coalesces them per frame.
 */
export function TerminalView({ onSink, onInput, onFit, onLink, onEngine, testID }: TerminalViewProps) {
  const web = useRef<WebView>(null);
  const ready = useRef(false);
  const queue = useRef<ToEngine[]>([]);
  const [ctrl, setCtrl] = useState(false);
  const readOnly = !onInput;

  const send = useCallback((msg: ToEngine) => {
    if (!ready.current) {
      queue.current.push(msg);
      return;
    }
    web.current?.injectJavaScript(injection(msg));
  }, []);

  const sink = useMemo<TerminalSink>(
    () => ({
      write: (bytes) => send({ t: "write", b64: toBase64(bytes) }),
      clear: () => send({ t: "clear" }),
    }),
    [send],
  );

  useEffect(() => {
    onSink(sink);
  }, [onSink, sink]);

  useEffect(() => {
    if (ready.current) send({ t: "readonly", on: readOnly });
  }, [readOnly, send]);

  const onMessage = (e: WebViewMessageEvent) => {
    // Only the inline document may talk to us. Any navigated-to page would
    // have the same bridge, so a foreign origin is dropped before decoding.
    if (!ENGINE_SOURCES.has(e.nativeEvent.url)) {
      onEngine?.(`dropped message from ${originOf(e.nativeEvent.url)}`);
      return;
    }
    const msg = decodeFromEngine(e.nativeEvent.data);
    if (!msg) return;
    if (msg.t === "ready") {
      ready.current = true;
      send({ t: "readonly", on: readOnly });
      for (const m of queue.current) send(m);
      queue.current = [];
      onEngine?.("engine ready");
    } else if (msg.t === "fit") onFit?.(msg.cols, msg.rows);
    else if (msg.t === "input") type(msg.data);
    else if (msg.t === "link") onLink?.(msg.uri);
    else if (msg.t === "stats") onEngine?.(`engine ready, ${msg.bytes} B, ${msg.cols}x${msg.rows}`);
  };

  const type = (data: string) => {
    if (!onInput) return;
    onInput(ctrl ? withCtrl(data) : data);
    if (ctrl) setCtrl(false);
  };

  return (
    <View style={styles.root} testID={testID}>
      <WebView
        ref={web}
        source={{ html: TERMINAL_HTML, baseUrl: ENGINE_URL }}
        onMessage={onMessage}
        onLoadStart={(e) => onEngine?.(`loading ${e.nativeEvent.url}`)}
        javaScriptEnabled
        scrollEnabled={false}
        hideKeyboardAccessoryView
        style={styles.web}
        testID="terminal-webview"
        {...LOCKDOWN}
      />
      {readOnly ? null : (
        <View style={styles.bar} testID="key-bar">
          {ACCESSORY_KEYS.map((k) => (
            <Pressable
              key={k.label}
              testID={`key-${k.label}`}
              onPress={() => (k.data === "ctrl" ? setCtrl((c) => !c) : type(k.data))}
              style={[styles.key, k.data === "ctrl" && ctrl && styles.keyOn]}
            >
              <Text style={styles.keyText}>{k.label}</Text>
            </Pressable>
          ))}
        </View>
      )}
    </View>
  );
}

const styles = StyleSheet.create({
  root: { flex: 1, backgroundColor: colors.bg },
  web: { flex: 1, backgroundColor: colors.bg },
  bar: { flexDirection: "row", flexWrap: "wrap", gap: 6, padding: 6, backgroundColor: colors.panel },
  key: { paddingHorizontal: 10, paddingVertical: 6, borderRadius: 6, borderWidth: 1, borderColor: colors.border },
  keyOn: { backgroundColor: colors.highlight, borderColor: colors.green },
  keyText: { color: colors.text, fontFamily: "monospace" },
});
