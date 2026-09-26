import { useRef, useState } from "react";
import { Modal, Pressable, StyleSheet, Text, TextInput, View } from "react-native";

import { colors } from "../theme";
import { useWire } from "../wire/context";
import type { AttentionRow } from "../wire/types";
import { outcomeCopy } from "./outcome";
import { retire } from "./store";

/**
 * The second tap. Options answer with their 1-based number (the picker and
 * bridge contract); anything without options takes free text. The op id is
 * minted by the crate on the first send and reused on every retry, so a lost
 * reply never becomes a second answer.
 */
export function AnswerSheet({ row, onClose }: { row: AttentionRow; onClose: () => void }) {
  const wire = useWire();
  const opId = useRef<string | undefined>(undefined);
  const [text, setText] = useState("");
  const [status, setStatus] = useState<{ text: string; final: boolean; lost?: string }>();
  const [busy, setBusy] = useState(false);

  const send = async (answer: string) => {
    if (busy) return;
    setBusy(true);
    try {
      opId.current ??= await wire.mintOpId();
      const reply = await wire.answer({ hostId: row.hostId, attentionId: row.id, answer, version: row.version, opId: opId.current });
      const copy = outcomeCopy(reply.outcome, reply.ack);
      setStatus(copy);
      if (copy.final) retire(row.id);
    } catch (e) {
      // Reply lost; the same op id goes back out on retry.
      setStatus({ text: `No reply (${e instanceof Error ? e.message : String(e)})`, final: false, lost: answer });
    } finally {
      setBusy(false);
    }
  };

  const options = row.payload.options ?? [];
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
          ) : status?.lost !== undefined ? (
            <Pressable testID="answer-retry" onPress={() => send(status.lost!)} style={styles.cta}>
              <Text style={styles.ctaText}>Retry</Text>
            </Pressable>
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
