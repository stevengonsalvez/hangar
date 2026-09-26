import { Link, useLocalSearchParams } from "expo-router";
import { useEffect } from "react";
import { FlatList, Pressable, StyleSheet, Text, View } from "react-native";

import { colors } from "../../../src/theme";
import { useWire, useWireQuery } from "../../../src/wire/context";

export default function Sessions() {
  const { hostId } = useLocalSearchParams<{ hostId: string }>();
  const wire = useWire();
  useEffect(() => {
    if (hostId) wire.connect(hostId).catch(() => undefined);
  }, [wire, hostId]);
  const { data, error } = useWireQuery((w) => (hostId ? w.rosterStatus(hostId) : Promise.resolve([])), [
    "fleet_revision",
  ]);
  return (
    <View style={styles.screen}>
      {error ? <Text style={styles.error}>{error}</Text> : null}
      <FlatList
        data={data ?? []}
        keyExtractor={(s) => s.sessionKey}
        ListEmptyComponent={<Text style={styles.muted}>No sessions.</Text>}
        renderItem={({ item }) => (
          <Link
            href={{ pathname: "/host/[hostId]/session/[key]", params: { hostId: item.hostId, key: item.sessionKey } }}
            asChild
          >
            <Pressable style={styles.row} testID={`session-${item.sessionKey}`}>
              <Text style={styles.name}>{item.name}</Text>
              <Text style={item.state === "ask" ? styles.ask : styles.muted}>
                {item.state} · {item.provenance} · {item.tier}
              </Text>
            </Pressable>
          </Link>
        )}
      />
    </View>
  );
}

const styles = StyleSheet.create({
  screen: { flex: 1, backgroundColor: colors.bg, padding: 12 },
  row: {
    padding: 12,
    marginBottom: 8,
    borderRadius: 8,
    borderWidth: 1,
    borderColor: colors.border,
    backgroundColor: colors.panel,
  },
  name: { color: colors.text, fontSize: 16, fontWeight: "600" },
  ask: { color: colors.gold },
  muted: { color: colors.muted },
  error: { color: colors.red },
});
