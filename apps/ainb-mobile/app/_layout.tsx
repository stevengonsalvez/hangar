import { Stack } from "expo-router";
import { StatusBar } from "expo-status-bar";

import { colors } from "../src/theme";
import { WireProvider } from "../src/wire/context";

export default function RootLayout() {
  return (
    <WireProvider>
      <StatusBar style="light" />
      <Stack
        screenOptions={{
          headerStyle: { backgroundColor: colors.panel },
          headerTintColor: colors.gold,
          contentStyle: { backgroundColor: colors.bg },
        }}
      >
        <Stack.Screen name="index" options={{ title: "Hosts" }} />
        <Stack.Screen name="pair" options={{ title: "Pair", presentation: "modal" }} />
        <Stack.Screen name="log" options={{ title: "Connection log" }} />
        <Stack.Screen name="host/[hostId]/index" options={{ title: "Sessions" }} />
        <Stack.Screen name="host/[hostId]/session/[key]" options={{ title: "Session" }} />
      </Stack>
    </WireProvider>
  );
}
