import { useLocalSearchParams, useRouter } from "expo-router";
import { useEffect, useState } from "react";
import { Pressable, StyleSheet, Text, TextInput, View } from "react-native";

import { useIncomingOffer } from "../src/pairing/deeplink";
import { Scanner } from "../src/pairing/Scanner";
import { colors } from "../src/theme";
import { useWire } from "../src/wire/context";
import { PEER_CHANGED } from "../src/wire/types";

/**
 * Three ways in (S1): scan the QR, open an `ainb://pair#` link, or paste the
 * text. The crate parses and redeems; this screen only reports the outcome.
 */
export default function Pair() {
  const wire = useWire();
  const router = useRouter();
  const incoming = useIncomingOffer();
  const { repair } = useLocalSearchParams<{ repair?: string }>();
  const [offer, setOffer] = useState("");
  const [name, setName] = useState("phone");
  const [status, setStatus] = useState<string>();
  const [scanning, setScanning] = useState(false);

  useEffect(() => {
    if (incoming) setOffer(incoming);
  }, [incoming]);

  const submit = async (uri = offer) => {
    try {
      const parsed = await wire.parseOffer(uri.trim());
      const paired = await wire.pair(parsed, name.trim() || "phone");
      setStatus(`paired as ${paired.deviceId} (${paired.scope.base})`);
      if (router.canGoBack()) router.back();
      else router.replace("/");
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setStatus(msg === PEER_CHANGED ? "This host's key changed. Ask the operator for a fresh offer." : msg);
    }
  };

  return (
    <View style={styles.screen}>
      {repair ? (
        <Text style={styles.repair} testID="repair-notice">
          {repair === "revoked" ? "This device was revoked. Pair again with a new offer." : "This pairing expired. Pair again with a new offer."}
        </Text>
      ) : null}
      {scanning ? (
        <Scanner
          onOffer={(uri) => {
            setScanning(false);
            setOffer(uri);
            void submit(uri);
          }}
        />
      ) : (
        <Pressable testID="scan" onPress={() => setScanning(true)} style={styles.secondary}>
          <Text style={styles.secondaryText}>Scan QR</Text>
        </Pressable>
      )}
      <Text style={styles.label}>Offer (ainb://pair#...)</Text>
      <TextInput
        testID="offer"
        style={styles.input}
        value={offer}
        onChangeText={setOffer}
        autoCapitalize="none"
        autoCorrect={false}
        multiline
      />
      <Text style={styles.label}>This device's name</Text>
      <TextInput testID="device-name" style={styles.input} value={name} onChangeText={setName} />
      <Pressable testID="pair-submit" style={styles.cta} onPress={() => void submit()}>
        <Text style={styles.ctaText}>Pair</Text>
      </Pressable>
      {status ? (
        <Text style={styles.status} testID="pair-status">
          {status}
        </Text>
      ) : null}
    </View>
  );
}

const styles = StyleSheet.create({
  screen: { flex: 1, backgroundColor: colors.bg, padding: 16, gap: 8 },
  label: { color: colors.muted },
  input: {
    color: colors.text,
    borderWidth: 1,
    borderColor: colors.border,
    borderRadius: 8,
    padding: 10,
    backgroundColor: colors.panel,
  },
  cta: { backgroundColor: colors.gold, borderRadius: 8, padding: 12, alignItems: "center", marginTop: 8 },
  ctaText: { color: colors.bg, fontWeight: "700" },
  secondary: { borderWidth: 1, borderColor: colors.border, borderRadius: 8, padding: 12, alignItems: "center" },
  secondaryText: { color: colors.text },
  status: { color: colors.text, marginTop: 8 },
  repair: { color: colors.gold },
});
