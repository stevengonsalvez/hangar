import { FlatList, StyleSheet, Text, View } from "react-native";

import { colors } from "../src/theme";
import { useWireQuery } from "../src/wire/context";

export default function Log() {
  const { data } = useWireQuery((w) => w.connectionLog(), ["closed", "reachability"]);
  return (
    <View style={styles.screen}>
      <FlatList
        data={data ?? []}
        keyExtractor={(_, i) => String(i)}
        ListEmptyComponent={<Text style={styles.muted}>Nothing logged yet.</Text>}
        renderItem={({ item }) => (
          <Text style={styles.line}>
            <Text style={styles.muted}>{new Date(item.atMs).toISOString()} </Text>
            {item.hostId ? <Text style={styles.host}>{item.hostId.slice(-5)} </Text> : null}
            <Text style={styles.event}>{item.event}</Text>
            {item.detail ? <Text style={styles.muted}> {item.detail}</Text> : null}
          </Text>
        )}
      />
    </View>
  );
}

const styles = StyleSheet.create({
  screen: { flex: 1, backgroundColor: colors.bg, padding: 12 },
  line: { color: colors.text, fontFamily: "monospace", fontSize: 12, marginBottom: 4 },
  host: { color: colors.border },
  event: { color: colors.gold },
  muted: { color: colors.muted },
});
