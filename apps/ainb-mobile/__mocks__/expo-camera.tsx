// jest stand-in for expo-camera: permission granted, and a test fires a scan
// through `camera.scan(text)`.
import React from "react";
import { View } from "react-native";

export const camera: { scan?: (data: string) => void; granted: boolean } = { scan: undefined, granted: true };

export function useCameraPermissions(): [{ granted: boolean; canAskAgain: boolean } | null, () => Promise<void>] {
  return [{ granted: camera.granted, canAskAgain: true }, async () => undefined];
}

export function CameraView(props: { testID?: string; onBarcodeScanned?: (r: { data: string }) => void }) {
  camera.scan = (data) => props.onBarcodeScanned?.({ data });
  return <View testID={props.testID} />;
}
