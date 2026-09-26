// Every test runs against the fake transport; the native binding is never
// loaded under jest.
process.env.EXPO_PUBLIC_FAKE_WIRE = "1";

// The native webview has no jest implementation; __mocks__/react-native-webview.tsx
// stands in and exposes the bridge to the terminal tests.
jest.mock("react-native-webview");
jest.mock("expo-camera");
