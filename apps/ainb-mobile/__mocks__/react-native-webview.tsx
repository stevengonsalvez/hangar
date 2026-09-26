// jest stand-in for the native webview: records what the host injects and
// lets a test play the engine's side of the bridge through `bridge`.
import React from "react";
import { View } from "react-native";

export const bridge: { injected: string[]; props?: Record<string, unknown>; engineMessage?: (raw: string, url?: string) => void; reset(): void } = {
  injected: [],
  engineMessage: undefined,
  reset() {
    this.injected = [];
    this.engineMessage = undefined;
  },
};

export const WebView = React.forwardRef(function WebView(
  props: { onMessage?: (e: { nativeEvent: { data: string; url: string } }) => void; testID?: string } & Record<string, unknown>,
  ref: React.Ref<{ injectJavaScript(js: string): void }>,
) {
  React.useImperativeHandle(ref, () => ({ injectJavaScript: (js: string) => bridge.injected.push(js) }));
  bridge.props = props;
  bridge.engineMessage = (raw, url = "about:blank") => props.onMessage?.({ nativeEvent: { data: raw, url } });
  return <View testID={props.testID ?? "terminal-webview"} />;
});

export type WebViewMessageEvent = { nativeEvent: { data: string; url: string } };
export default WebView;
