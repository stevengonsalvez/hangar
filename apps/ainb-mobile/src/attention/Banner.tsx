import { useCallback, useState } from "react";
import { Pressable, StyleSheet, Text } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";

import { colors } from "../theme";
import { useWireEvents } from "../wire/context";
import { AnswerSheet } from "./AnswerSheet";
import type { AttentionRow } from "../wire/types";
import { raise, retire, useAttentionRows } from "./store";

/**
 * The first tap. One in-app banner per open attention row, newest on top,
 * announced once per id (S9). Raised from the live socket; the lifecycle's
 * reconcile on every connect covers rows already open when we connected.
 */
export function Banner() {
  const rows = useAttentionRows();
  const insets = useSafeAreaInsets();
  // The sheet keeps the row it opened with: a retire while it is up must not
  // pull the outcome copy out from under the reader.
  const [opened, setOpened] = useState<AttentionRow>();

  useWireEvents(
    useCallback((ev) => {
      if (ev.kind === "attention_raised") raise(ev.row);
      if (ev.kind === "attention_answered") retire(ev.hostId, ev.attentionId);
    }, []),
  );

  const top = rows.at(-1);
  const title = top ? (top.payload.question ?? top.payload.text ?? top.kind) : "";
  return (
    <>
      {top ? (
        // The label repeats the title: an accessible Pressable hides its child
        // texts from the accessibility tree (screen readers and Maestro alike).
        <Pressable
          testID={`banner-${top.id}`}
          accessibilityRole="button"
          accessibilityLabel={title}
          onPress={() => setOpened(top)}
          style={[styles.banner, { top: insets.top + 8 }]}
        >
          <Text style={styles.title} numberOfLines={1}>
            {title}
          </Text>
          <Text style={styles.sub}>
            {rows.length > 1 ? `${rows.length} waiting · ` : ""}
            {top.sessionKey ?? top.sessionId}
          </Text>
        </Pressable>
      ) : null}
      {opened ? <AnswerSheet row={opened} onClose={() => setOpened(undefined)} /> : null}
    </>
  );
}

const styles = StyleSheet.create({
  banner: { position: "absolute", left: 8, right: 8, padding: 12, borderRadius: 10, backgroundColor: colors.gold, zIndex: 10 },
  title: { color: colors.bg, fontWeight: "700" },
  sub: { color: colors.bg, fontSize: 12 },
});
