import { useCallback, useEffect, useState } from "react";
import { Pressable, StyleSheet, Text } from "react-native";

import { colors } from "../theme";
import { useWire, useWireEvents } from "../wire/context";
import { AnswerSheet } from "./AnswerSheet";
import type { AttentionRow } from "../wire/types";
import { raise, reconcile, retire, useAttentionRows } from "./store";

/**
 * The first tap. One in-app banner per open attention row, newest on top,
 * shown once per id (S9). Raised from the live socket; the initial pull per
 * host covers rows that were already open when we connected.
 */
export function Banner() {
  const wire = useWire();
  const rows = useAttentionRows();
  // The sheet keeps the row it opened with: a retire while it is up must not
  // pull the outcome copy out from under the reader.
  const [opened, setOpened] = useState<AttentionRow>();

  useEffect(() => {
    wire
      .hosts()
      .then((hosts) => Promise.all(hosts.map((h) => reconcile(wire, h.hostId).catch(() => undefined))))
      .catch(() => undefined);
  }, [wire]);

  useWireEvents(
    useCallback((ev) => {
      if (ev.kind === "attention_raised") raise(ev.row);
      if (ev.kind === "attention_answered") retire(ev.hostId, ev.attentionId);
    }, []),
  );

  const top = rows.at(-1);
  return (
    <>
      {top ? (
        <Pressable testID={`banner-${top.id}`} onPress={() => setOpened(top)} style={styles.banner}>
          <Text style={styles.title} numberOfLines={1}>
            {top.payload.question ?? top.payload.text ?? top.kind}
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
  banner: { position: "absolute", top: 8, left: 8, right: 8, padding: 12, borderRadius: 10, backgroundColor: colors.gold, zIndex: 10 },
  title: { color: colors.bg, fontWeight: "700" },
  sub: { color: colors.bg, fontSize: 12 },
});
