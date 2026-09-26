import { useURL } from "expo-linking";

/** The offer an `ainb://pair#...` link opened the app with, if any. */
export function useIncomingOffer(): string | undefined {
  const url = useURL();
  return url && url.startsWith("ainb://pair#") ? url : undefined;
}
