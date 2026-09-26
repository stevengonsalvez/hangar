// Every test runs against the fake transport; the native binding is never
// loaded under jest.
process.env.EXPO_PUBLIC_FAKE_WIRE = "1";
