import { useState } from "react";
import { Pressable, StyleSheet, Text, View } from "react-native";

import { colors } from "../theme";
import type { HostId, SessionKey } from "../wire/types";
import { TerminalView } from "./TerminalView";
import { useTerminal } from "./useTerminal";

/** Only the host of an ignored link is shown; the rest of the URI never leaves the engine. */
function hostOf(uri: string): string {
  const m = /^[a-z][a-z0-9+.-]*:\/\/([^/?#]+)/i.exec(uri);
  return m?.[1] ?? "(no host)";
}

/** The Terminal tab: read only under `mobile`, a type toggle under `mobile+type`. */
export function WireTerminal({ hostId, sessionKey }: { hostId?: HostId; sessionKey?: SessionKey }) {
  const { state, setSink, input, setTyping, take, fit } = useTerminal(hostId, sessionKey);
  const [engine, setEngine] = useState("engine loading");
  const [link, setLink] = useState<string>();
  return (
    <View style={styles.root}>
      <View style={styles.bar}>
        <Text style={styles.muted} testID="terminal-size">
          {state.cols && state.rows ? `${state.cols}x${state.rows}` : ""}
        </Text>
        <Text style={styles.muted} testID="terminal-engine" accessibilityLabel={engine}>
          {engine}
        </Text>
        {state.nativeClients > 0 ? (
          <Text style={styles.badge} testID="native-badge">
            native client attached, input not arbitrated
          </Text>
        ) : null}
        {state.canType ? (
          <Pressable testID="type-toggle" onPress={() => setTyping(!state.typing)} style={[styles.toggle, state.typing && styles.toggleOn]}>
            <Text style={state.typing ? styles.toggleTextOn : styles.toggleText}>{state.typing ? "typing" : "type"}</Text>
          </Pressable>
        ) : (
          <Text style={styles.muted} testID="read-only">
            read only
          </Text>
        )}
      </View>
      {state.denied ? (
        <View style={styles.denied} testID="floor-denied">
          <Text style={styles.deniedText}>{state.denied.label} has the floor</Text>
          <Pressable testID="take-over" onPress={take} style={styles.take}>
            <Text style={styles.takeText}>Take over</Text>
          </Pressable>
        </View>
      ) : null}
      {link ? (
        <Text style={styles.link} testID="link-ignored" accessibilityLabel={`link ignored: ${link}`}>
          link ignored: {link}
        </Text>
      ) : null}
      {state.closed ? (
        <Text style={styles.closed} testID="terminal-closed">
          closed: {state.closed}
        </Text>
      ) : null}
      <TerminalView
        testID="terminal"
        onSink={setSink}
        onInput={state.canType && state.typing ? input : undefined}
        onFit={fit}
        onEngine={(st) => setEngine((prev) => (prev === st ? prev : st))}
        onLink={(uri) => setLink(hostOf(uri))}
      />
    </View>
  );
}

const styles = StyleSheet.create({
  root: { flex: 1 },
  bar: { flexDirection: "row", alignItems: "center", gap: 8, paddingHorizontal: 8, paddingVertical: 4 },
  muted: { color: colors.muted, fontSize: 12 },
  badge: { color: colors.gold, fontSize: 11, flexShrink: 1 },
  toggle: { marginLeft: "auto", paddingHorizontal: 10, paddingVertical: 4, borderRadius: 6, borderWidth: 1, borderColor: colors.border },
  toggleOn: { borderColor: colors.green, backgroundColor: colors.highlight },
  toggleText: { color: colors.muted },
  toggleTextOn: { color: colors.green },
  denied: { flexDirection: "row", alignItems: "center", gap: 8, padding: 8, backgroundColor: colors.highlight },
  deniedText: { color: colors.red, flex: 1 },
  take: { backgroundColor: colors.gold, borderRadius: 6, paddingHorizontal: 10, paddingVertical: 4 },
  takeText: { color: colors.bg, fontWeight: "700" },
  closed: { color: colors.red, paddingHorizontal: 8 },
  link: { color: colors.gold, paddingHorizontal: 8 },
});
