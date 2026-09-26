import { useLocalSearchParams, useRouter } from "expo-router";
import { useEffect, useState } from "react";
import { Pressable, StyleSheet, Text, TextInput, View } from "react-native";

import { useIncomingOffer } from "../src/pairing/deeplink";
import { Scanner } from "../src/pairing/Scanner";
import { connectHost } from "../src/lifecycle";
import { colors } from "../src/theme";
import { useWire } from "../src/wire/context";
import { PEER_CHANGED, type PairingOffer } from "../src/wire/types";

/**
 * Three ways in (S1): scan the QR, open an `ainb://pair#` link, or paste the
 * text. Every way fills the field and shows the decoded host and expiry; the
 * Pair tap redeems (the crate reads the secret from the URI). A hostile QR
 * therefore never pairs in one tap.
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
  const [preview, setPreview] = useState<PairingOffer>();

  useEffect(() => {
    if (incoming) setOffer(incoming);
  }, [incoming]);

  useEffect(() => {
    const uri = offer.trim();
    if (!uri) return setPreview(undefined);
    let live = true;
    wire.parseOffer(uri).then(
      (p) => live && setPreview(p),
      () => live && setPreview(undefined),
    );
    return () => {
      live = false;
    };
  }, [wire, offer]);

  const submit = async () => {
    try {
      const paired = await wire.pair(offer.trim(), name.trim() || "phone");
      setStatus(`paired as ${paired.deviceId} (${paired.scope.base})`);
      void connectHost(wire, paired.hostId).catch(() => undefined);
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
          {repair === "revoked"
            ? "This device was revoked or its pairing expired. Pair again with a new offer."
            : repair === "peer_changed"
              ? "This host's key changed. Ask the operator for a fresh offer and pair again."
              : repair === "unauthenticated"
                ? "This host no longer accepts this device. Pair again with a new offer."
                : `This pairing needs redoing (${repair}). Pair again with a new offer.`}
        </Text>
      ) : null}
      {scanning ? (
        <Scanner
          onOffer={(uri) => {
            setScanning(false);
            setOffer(uri);
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
      {preview ? (
        <Text style={styles.preview} testID="offer-preview">
          host {preview.hostId} via {preview.endpoints.map((e) => e.carrier).join(", ")}, expires{" "}
          {new Date(preview.expiresAtMs).toLocaleTimeString()}
        </Text>
      ) : null}
      <Text style={styles.label}>This device's name</Text>
      <TextInput testID="device-name" style={styles.input} value={name} onChangeText={setName} />
      <Pressable testID="pair-submit" style={[styles.cta, !preview && styles.ctaOff]} disabled={!preview} onPress={() => void submit()}>
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
  ctaOff: { opacity: 0.4 },
  ctaText: { color: colors.bg, fontWeight: "700" },
  preview: { color: colors.text, fontFamily: "monospace", fontSize: 12 },
  secondary: { borderWidth: 1, borderColor: colors.border, borderRadius: 8, padding: 12, alignItems: "center" },
  secondaryText: { color: colors.text },
  status: { color: colors.text, marginTop: 8 },
  repair: { color: colors.gold },
});
