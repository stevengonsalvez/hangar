// jest stand-in for the native webview: records what the host injects and
// lets a test play the engine's side of the bridge through `bridge`.
import React from "react";
import { View } from "react-native";

export const bridge: { injected: string[]; engineMessage?: (raw: string) => void; reset(): void } = {
  injected: [],
  engineMessage: undefined,
  reset() {
    this.injected = [];
    this.engineMessage = undefined;
  },
};

export const WebView = React.forwardRef(function WebView(
  props: { onMessage?: (e: { nativeEvent: { data: string } }) => void; testID?: string },
  ref: React.Ref<{ injectJavaScript(js: string): void }>,
) {
  React.useImperativeHandle(ref, () => ({ injectJavaScript: (js: string) => bridge.injected.push(js) }));
  bridge.engineMessage = (raw) => props.onMessage?.({ nativeEvent: { data: raw } });
  return <View testID={props.testID ?? "terminal-webview"} />;
});

export type WebViewMessageEvent = { nativeEvent: { data: string } };
export default WebView;
