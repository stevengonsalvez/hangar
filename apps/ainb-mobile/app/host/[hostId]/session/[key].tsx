import { useLocalSearchParams } from "expo-router";
import { useCallback, useState } from "react";
import { FlatList, Pressable, StyleSheet, Text, TextInput, View } from "react-native";

import { colors } from "../../../../src/theme";
import { useWire, useWireEvents, useWireQuery } from "../../../../src/wire/context";
import type { TranscriptEntry } from "../../../../src/wire/types";

type Tab = "transcript" | "terminal";

export default function Session() {
  const { hostId, key } = useLocalSearchParams<{ hostId: string; key: string }>();
  const wire = useWire();
  const [tab, setTab] = useState<Tab>("transcript");
  const [tail, setTail] = useState<TranscriptEntry[]>([]);
  const [draft, setDraft] = useState("");
  const [notice, setNotice] = useState<string>();

  const page = useWireQuery((w) => (hostId && key ? w.transcriptPage(hostId, key) : Promise.resolve([])));
  const roster = useWireQuery((w) => (hostId ? w.rosterStatus(hostId) : Promise.resolve([])), ["fleet_revision"]);
  const row = roster.data?.find((s) => s.sessionKey === key);

  useWireEvents(
    useCallback(
      (ev) => {
        if (ev.kind === "transcript_line" && ev.hostId === hostId && ev.sessionKey === key)
          setTail((t) => [...t, ev.entry]);
      },
      [hostId, key],
    ),
  );

  const entries = [...(page.data ?? []), ...tail];

  const send = async () => {
    if (!hostId || !key || !row || !draft.trim()) return;
    const ack = await wire.sendPrompt({ hostId, sessionKey: key, text: draft.trim(), lifecycleUpdatedAt: row.lifecycleUpdatedAt });
    setNotice(ack.status === "accepted" ? undefined : `Not sent: ${ack.reason ?? ack.status}`);
    if (ack.status === "accepted") setDraft("");
  };

  const interrupt = async () => {
    if (!hostId || !key || !row) return;
    const ack = await wire.interrupt({ hostId, sessionKey: key, sessionIncarnation: row.sessionIncarnation });
    setNotice(ack.status === "accepted" ? "Interrupted" : `Not interrupted: ${ack.reason ?? ack.status}`);
  };

  return (
    <View style={styles.screen}>
      <View style={styles.tabs}>
        {(["transcript", "terminal"] as Tab[]).map((t) => (
          <Pressable key={t} testID={`tab-${t}`} onPress={() => setTab(t)} style={[styles.tab, tab === t && styles.tabOn]}>
            <Text style={tab === t ? styles.tabTextOn : styles.tabText}>{t}</Text>
          </Pressable>
        ))}
        <Pressable testID="interrupt" onPress={interrupt} style={styles.interrupt}>
          <Text style={styles.interruptText}>Interrupt</Text>
        </Pressable>
      </View>
      {notice ? <Text style={styles.notice}>{notice}</Text> : null}
      {tab === "transcript" ? (
        <>
          <FlatList
            data={entries}
            keyExtractor={(e) => String(e.seq)}
            renderItem={({ item }) => (
              <Text style={styles.entry}>
                <Text style={item.role === "user" ? styles.user : styles.agent}>{item.role} </Text>
                {item.text}
              </Text>
            )}
          />
          <View style={styles.compose}>
            <TextInput testID="prompt" style={styles.input} value={draft} onChangeText={setDraft} placeholder="prompt" placeholderTextColor={colors.muted} />
            <Pressable testID="send" onPress={send} style={styles.send}>
              <Text style={styles.sendText}>Send</Text>
            </Pressable>
          </View>
        </>
      ) : (
        <View style={styles.terminal} testID="terminal-placeholder">
          <Text style={styles.muted}>Terminal lands with the stream (M1-11, M1-13).</Text>
        </View>
      )}
    </View>
  );
}

const styles = StyleSheet.create({
  screen: { flex: 1, backgroundColor: colors.bg },
  tabs: { flexDirection: "row", alignItems: "center", padding: 8, gap: 8 },
  tab: { paddingHorizontal: 12, paddingVertical: 6, borderRadius: 6, borderWidth: 1, borderColor: colors.border },
  tabOn: { backgroundColor: colors.highlight, borderColor: colors.green },
  tabText: { color: colors.muted },
  tabTextOn: { color: colors.green },
  interrupt: { marginLeft: "auto", paddingHorizontal: 12, paddingVertical: 6 },
  interruptText: { color: colors.red },
  notice: { color: colors.gold, paddingHorizontal: 12 },
  entry: { color: colors.text, paddingHorizontal: 12, paddingVertical: 4 },
  user: { color: colors.gold },
  agent: { color: colors.border },
  compose: { flexDirection: "row", padding: 8, gap: 8 },
  input: { flex: 1, color: colors.text, borderWidth: 1, borderColor: colors.border, borderRadius: 8, padding: 10, backgroundColor: colors.panel },
  send: { backgroundColor: colors.gold, borderRadius: 8, paddingHorizontal: 14, justifyContent: "center" },
  sendText: { color: colors.bg, fontWeight: "700" },
  terminal: { flex: 1, alignItems: "center", justifyContent: "center" },
  muted: { color: colors.muted },
});
