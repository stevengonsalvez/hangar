import { useLocalSearchParams } from "expo-router";
import { useCallback, useEffect, useRef, useState } from "react";
import { FlatList, Pressable, StyleSheet, Text, TextInput, View } from "react-native";

import { connectHost } from "../../../../src/lifecycle";
import { WireTerminal } from "../../../../src/terminal/WireTerminal";
import { colors } from "../../../../src/theme";
import { useWire, useWireQuery } from "../../../../src/wire/context";
import type { TranscriptEntry } from "../../../../src/wire/types";

type Tab = "transcript" | "terminal";

export default function Session() {
  const { hostId, key } = useLocalSearchParams<{ hostId: string; key: string }>();
  const wire = useWire();
  const [tab, setTab] = useState<Tab>("transcript");
  const [tail, setTail] = useState<TranscriptEntry[]>([]);
  const [draft, setDraft] = useState("");
  const [notice, setNotice] = useState<string>();
  // One op id per draft: a retry of the same text reuses it, new text mints anew.
  const draftOp = useRef<{ text: string; opId: string } | undefined>(undefined);
  const inFlight = useRef(false);
  useEffect(() => {
    if (hostId) connectHost(wire, hostId).catch(() => undefined);
  }, [wire, hostId]);

  const page = useWireQuery((w) => (hostId && key ? w.transcriptPage(hostId, key) : Promise.resolve([])));
  const roster = useWireQuery((w) => (hostId ? w.rosterStatus(hostId) : Promise.resolve([])), ["fleet_revision"]);
  const row = roster.data?.find((s) => s.sessionKey === key);

  useEffect(() => {
    if (!hostId || !key) return;
    return wire.subscribeTranscript(hostId, key, (entry) => setTail((t) => [...t, entry]));
  }, [wire, hostId, key]);

  // Page and tail can overlap when a live line lands before the page: merge by seq.
  const bySeq = new Map<number, TranscriptEntry>();
  for (const e of [...(page.data ?? []), ...tail]) bySeq.set(e.seq, e);
  const entries = [...bySeq.values()].sort((a, b) => a.seq - b.seq);

  const guarded = async (run: () => Promise<void>) => {
    if (inFlight.current) return;
    inFlight.current = true;
    try {
      await run();
    } catch (e) {
      setNotice(`No reply, tap again to retry (${e instanceof Error ? e.message : String(e)})`);
    } finally {
      inFlight.current = false;
    }
  };

  const send = () =>
    guarded(async () => {
      const text = draft.trim();
      if (!hostId || !key || !row || !text) return;
      if (draftOp.current?.text !== text) draftOp.current = { text, opId: await wire.mintOpId() };
      const ack = await wire.sendPrompt({ hostId, sessionKey: key, text, lifecycleUpdatedAt: row.lifecycleUpdatedAt, opId: draftOp.current.opId });
      setNotice(ack.status === "accepted" ? undefined : `Not sent: ${ack.reason ?? ack.status}`);
      if (ack.status === "accepted") {
        setDraft("");
        draftOp.current = undefined;
      }
    });

  // One op id per row version: a retry of the same view reuses it, a new version mints anew,
  // so a stale interrupt can never replay under an id minted for an older row.
  const interruptOp = useRef<{ version: number; opId: string } | undefined>(undefined);
  const interrupt = () =>
    guarded(async () => {
      if (!hostId || !key || !row) return;
      if (interruptOp.current?.version !== row.version) interruptOp.current = { version: row.version, opId: await wire.mintOpId() };
      // the version of the row the user saw: a stale view is refused, never a null on the wire
      const ack = await wire.interrupt({ hostId, sessionKey: key, sessionIncarnation: row.sessionIncarnation, version: row.version, opId: interruptOp.current.opId });
      if (ack.status === "accepted") interruptOp.current = undefined;
      setNotice(ack.status === "accepted" ? "Interrupted" : `Not interrupted: ${ack.reason ?? ack.status}`);
    });

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
        <WireTerminal hostId={hostId} sessionKey={key} />
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
});
