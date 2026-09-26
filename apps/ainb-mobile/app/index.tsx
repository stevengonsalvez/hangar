import { Link } from "expo-router";
import { FlatList, Pressable, StyleSheet, Text, View } from "react-native";

import { colors } from "../src/theme";
import { useWireQuery } from "../src/wire/context";

/** Copy for the latch values lane E's record can carry; an unknown value still shows and still blocks. */
const REPAIR_COPY: Record<string, string> = {
  revoked: "revoked or expired, pair again",
  identity: "host no longer accepts this device, pair again",
  peer_changed: "host key changed, pair again with a fresh offer",
};
const NOTICE_COPY: Record<string, string> = {
  update_required: "update the app or the host",
  unknown_close: "closed with an unknown code, check the host",
};

export default function Hosts() {
  const { data: hosts, error } = useWireQuery((w) => w.hosts(), ["reachability", "closed"]);
  return (
    <View style={styles.screen}>
      {error ? <Text style={styles.error}>{error}</Text> : null}
      <FlatList
        data={hosts ?? []}
        keyExtractor={(h) => h.hostId}
        ListEmptyComponent={<Text style={styles.muted}>No paired hosts yet.</Text>}
        renderItem={({ item }) => (
          // A latched 4403 / 4401 sends the row to the pair screen instead of the host.
          <Link
            href={item.repair ? { pathname: "/pair", params: { repair: item.repair } } : { pathname: "/host/[hostId]", params: { hostId: item.hostId } }}
            asChild
          >
            <Pressable
              style={styles.row}
              testID={`host-${item.hostId}`}
              accessibilityRole="button"
              accessibilityLabel={`${item.displayName}, ${item.repair ?? item.notice ?? item.reachability}`}
            >
              <Text style={styles.name}>{item.displayName}</Text>
              {item.repair ? (
                <Text style={styles.repair} testID={`repair-${item.hostId}`}>
                  {REPAIR_COPY[item.repair] ?? `pair again (${item.repair})`}
                </Text>
              ) : item.notice ? (
                <Text style={styles.repair} testID={`notice-${item.hostId}`}>
                  {NOTICE_COPY[item.notice] ?? `check the host (${item.notice})`}
                </Text>
              ) : (
                <Text style={item.reachability === "reachable" ? styles.up : styles.down}>
                  {item.reachability === "reachable"
                    ? "reachable"
                    : item.reachability === "stale"
                      ? `stale since ${new Date(item.sinceMs ?? 0).toLocaleTimeString()}`
                      : item.reachability === "unreachable"
                        ? `unreachable since ${new Date(item.sinceMs ?? 0).toLocaleTimeString()}`
                        : "unknown"}
                </Text>
              )}
            </Pressable>
          </Link>
        )}
      />
      <View style={styles.bar}>
        <Link href="/pair" asChild>
          <Pressable testID="pair" style={styles.cta}>
            <Text style={styles.ctaText}>Pair a host</Text>
          </Pressable>
        </Link>
        <Link href="/log" asChild>
          <Pressable testID="log">
            <Text style={styles.muted}>Log</Text>
          </Pressable>
        </Link>
      </View>
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
  up: { color: colors.green },
  down: { color: colors.muted },
  repair: { color: colors.gold },
  muted: { color: colors.muted },
  error: { color: colors.red },
  bar: { flexDirection: "row", justifyContent: "space-between", alignItems: "center", paddingTop: 8 },
  cta: { backgroundColor: colors.gold, borderRadius: 8, paddingHorizontal: 16, paddingVertical: 10 },
  ctaText: { color: colors.bg, fontWeight: "700" },
});
