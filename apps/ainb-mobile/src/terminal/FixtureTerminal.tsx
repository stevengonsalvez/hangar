import { useState } from "react";
import { Pressable, StyleSheet, Text, View } from "react-native";

import { colors } from "../theme";
import { fromBase64 } from "./engine/protocol";
import { FIXTURES, type FixtureName } from "./fixtures";
import { TerminalView, type TerminalSink } from "./TerminalView";

/** The terminal tab before the stream exists: replays a recorded fixture. */
export function FixtureTerminal() {
  const [name, setName] = useState<FixtureName>("f1-altscreen");
  const [sink, setSink] = useState<TerminalSink>();
  const [typed, setTyped] = useState("");

  const play = (n: FixtureName, s = sink) => {
    setName(n);
    if (!s) return;
    s.clear();
    s.write(fromBase64(FIXTURES[n]));
  };

  return (
    <View style={styles.root}>
      <View style={styles.bar}>
        {(Object.keys(FIXTURES) as FixtureName[]).map((n) => (
          <Pressable key={n} testID={`fixture-${n}`} onPress={() => play(n)} style={[styles.chip, n === name && styles.chipOn]}>
            <Text style={n === name ? styles.chipTextOn : styles.chipText}>{n}</Text>
          </Pressable>
        ))}
        {typed ? (
          <Text style={styles.typed} testID="typed">
            {JSON.stringify(typed)}
          </Text>
        ) : null}
      </View>
      <TerminalView
        testID="terminal"
        onReady={(s) => {
          setSink(s);
          play(name, s);
        }}
        onInput={(d) => setTyped((t) => (t + d).slice(-24))}
      />
    </View>
  );
}

const styles = StyleSheet.create({
  root: { flex: 1 },
  bar: { flexDirection: "row", alignItems: "center", gap: 6, padding: 6, flexWrap: "wrap" },
  chip: { paddingHorizontal: 10, paddingVertical: 4, borderRadius: 6, borderWidth: 1, borderColor: colors.border },
  chipOn: { borderColor: colors.green, backgroundColor: colors.highlight },
  chipText: { color: colors.muted, fontSize: 12 },
  chipTextOn: { color: colors.green, fontSize: 12 },
  typed: { color: colors.gold, fontFamily: "monospace", fontSize: 12, marginLeft: "auto" },
});
