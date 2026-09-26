import { useRef, useState } from "react";
import { Modal, Pressable, StyleSheet, Text, TextInput, View } from "react-native";

import { colors } from "../theme";
import { useWire } from "../wire/context";
import type { AttentionRow } from "../wire/types";
import { outcomeCopy } from "./outcome";
import { markSent, retire, sentFor } from "./store";

/**
 * The second tap. Options answer with their 1-based number (the picker and
 * bridge contract); anything without options takes free text. The first send
 * pins `{op id, answer}` for this row in the store, so a retry from any later
 * sheet resends that answer under that id and nothing else (D18).
 */
export function AnswerSheet({ row, onClose }: { row: AttentionRow; onClose: () => void }) {
  const wire = useWire();
  const inFlight = useRef(false);
  const [text, setText] = useState("");
  const [status, setStatus] = useState<{ text: string; final: boolean; lost?: boolean }>();
  const [, bump] = useState(0);
  const sent = sentFor(row.hostId, row.id);

  const send = async (answer: string) => {
    if (inFlight.current) return;
    inFlight.current = true;
    try {
      let pinned = sentFor(row.hostId, row.id);
      if (pinned && pinned.answer !== answer) return; // only the pinned answer may go out
      if (!pinned) {
        pinned = { opId: await wire.mintOpId(), answer };
        markSent(row.hostId, row.id, pinned);
        bump((n) => n + 1);
      }
      const reply = await wire.answer({ hostId: row.hostId, attentionId: row.id, answer: pinned.answer, version: row.version, opId: pinned.opId });
      const copy = outcomeCopy(reply.outcome, reply.ack);
      setStatus(copy);
      if (copy.retire) retire(row.hostId, row.id);
    } catch (e) {
      // Reply lost; the pinned answer goes back out under the pinned op id.
      setStatus({ text: `No reply (${e instanceof Error ? e.message : String(e)})`, final: false, lost: true });
    } finally {
      inFlight.current = false;
    }
  };

  const options = row.payload.options ?? [];
  const pinnedLabel = sent ? (options[Number(sent.answer) - 1] ?? sent.answer) : undefined;
  return (
    <Modal transparent animationType="slide" onRequestClose={onClose}>
      <View style={styles.backdrop}>
        <View style={styles.sheet} testID="answer-sheet">
          <Text style={styles.kind}>{row.kind}</Text>
          <Text style={styles.question}>{row.payload.question ?? row.payload.text ?? ""}</Text>
          {status ? (
            <Text style={styles.status} testID="answer-status">
              {status.text}
            </Text>
          ) : null}
          {status?.final ? (
            <Pressable testID="answer-done" onPress={onClose} style={styles.cta}>
              <Text style={styles.ctaText}>Done</Text>
            </Pressable>
          ) : sent ? (
            <>
              <Text style={styles.status} testID="answer-pinned">
                Sent: {pinnedLabel}
              </Text>
              <Pressable testID="answer-retry" onPress={() => send(sent.answer)} style={styles.cta}>
                <Text style={styles.ctaText}>{status?.lost ? "Retry" : "Check again"}</Text>
              </Pressable>
              <Pressable testID="answer-cancel" onPress={onClose}>
                <Text style={styles.muted}>Not now</Text>
              </Pressable>
            </>
          ) : (
            <>
              {options.map((label, i) => (
                <Pressable key={i} testID={`option-${i + 1}`} onPress={() => send(String(i + 1))} style={styles.option}>
                  <Text style={styles.optionText}>
                    {i + 1}. {label}
                  </Text>
                </Pressable>
              ))}
              {options.length === 0 ? (
                <View style={styles.reply}>
                  <TextInput testID="answer-text" style={styles.input} value={text} onChangeText={setText} placeholder="reply" placeholderTextColor={colors.muted} />
                  <Pressable testID="answer-send" onPress={() => text.trim() && send(text.trim())} style={styles.cta}>
                    <Text style={styles.ctaText}>Send</Text>
                  </Pressable>
                </View>
              ) : null}
              <Pressable testID="answer-cancel" onPress={onClose}>
                <Text style={styles.muted}>Not now</Text>
              </Pressable>
            </>
          )}
        </View>
      </View>
    </Modal>
  );
}

const styles = StyleSheet.create({
  backdrop: { flex: 1, justifyContent: "flex-end", backgroundColor: "rgba(0,0,0,0.5)" },
  sheet: { backgroundColor: colors.panel, borderTopLeftRadius: 16, borderTopRightRadius: 16, padding: 16, gap: 10, borderColor: colors.border, borderWidth: 1 },
  kind: { color: colors.muted, fontSize: 12 },
  question: { color: colors.text, fontSize: 16 },
  status: { color: colors.gold },
  option: { padding: 12, borderRadius: 8, borderWidth: 1, borderColor: colors.border, backgroundColor: colors.highlight },
  optionText: { color: colors.text },
  reply: { flexDirection: "row", gap: 8 },
  input: { flex: 1, color: colors.text, borderWidth: 1, borderColor: colors.border, borderRadius: 8, padding: 10 },
  cta: { backgroundColor: colors.gold, borderRadius: 8, padding: 12, alignItems: "center" },
  ctaText: { color: colors.bg, fontWeight: "700" },
  muted: { color: colors.muted, textAlign: "center", padding: 8 },
});
