import { CameraView, useCameraPermissions } from "expo-camera";
import { useRef } from "react";
import { Pressable, StyleSheet, Text, View } from "react-native";

import { colors } from "../theme";

/** QR scan of a pairing offer; hands the raw `ainb://pair#...` text up once. */
export function Scanner({ onOffer }: { onOffer(uri: string): void }) {
  const [permission, request] = useCameraPermissions();
  // The scanner fires on every frame; one offer is handed up, then it is done.
  const done = useRef(false);
  if (!permission?.granted) {
    return (
      <Pressable testID="camera-permission" onPress={() => void request()} style={styles.ask}>
        <Text style={styles.askText}>{permission?.canAskAgain === false ? "Camera access is off in Settings" : "Allow camera to scan"}</Text>
      </Pressable>
    );
  }
  return (
    <View style={styles.frame}>
      <CameraView
        testID="camera"
        style={StyleSheet.absoluteFill}
        facing="back"
        barcodeScannerSettings={{ barcodeTypes: ["qr"] }}
        onBarcodeScanned={({ data }) => {
          if (done.current || !data.startsWith("ainb://pair#")) return;
          done.current = true;
          onOffer(data);
        }}
      />
    </View>
  );
}

const styles = StyleSheet.create({
  frame: { height: 220, borderRadius: 8, overflow: "hidden", borderWidth: 1, borderColor: colors.border },
  ask: { padding: 12, borderRadius: 8, borderWidth: 1, borderColor: colors.border, alignItems: "center" },
  askText: { color: colors.muted },
});
