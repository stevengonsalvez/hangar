import { useRouter } from "expo-router";
import { useState } from "react";
import { Pressable, StyleSheet, Text, TextInput, View } from "react-native";

import { colors } from "../src/theme";
import { useWire } from "../src/wire/context";

// Paste or deep-link only for now. QR scan (expo-camera) arrives with the
// native binding in M1-12; the crate does the parse either way.
export default function Pair() {
  const wire = useWire();
  const router = useRouter();
  const [offer, setOffer] = useState("");
  const [name, setName] = useState("phone");
  const [status, setStatus] = useState<string>();

  const submit = async () => {
    try {
      const parsed = await wire.parseOffer(offer.trim());
      const paired = await wire.pair(parsed, name.trim() || "phone");
      setStatus(`paired as ${paired.deviceId} (${paired.scope.base})`);
      if (router.canGoBack()) router.back();
      else router.replace("/");
    } catch (e) {
      setStatus(String(e instanceof Error ? e.message : e));
    }
  };

  return (
    <View style={styles.screen}>
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
      <Pressable testID="pair-submit" style={styles.cta} onPress={submit}>
        <Text style={styles.ctaText}>Pair</Text>
      </Pressable>
      {status ? <Text style={styles.status}>{status}</Text> : null}
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
  status: { color: colors.text, marginTop: 8 },
});
