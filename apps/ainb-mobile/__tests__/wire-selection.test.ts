import { FakeWire } from "../src/wire/fake";
import { isNativeModule, setWire, wire } from "../src/wire";
import { pathOf, resetWirePaths, wirePaths } from "../src/wire/paths";

jest.mock("expo-file-system");

beforeEach(() => {
  setWire(undefined);
  resetWirePaths();
});

test("the fake switch still selects FakeWire and never touches the file system or the adapter", () => {
  process.env.EXPO_PUBLIC_FAKE_WIRE = "1";
  expect(wire()).toBeInstanceOf(FakeWire);
});

test("without the switch, a missing adapter is a loud failure, not a silent fake", () => {
  process.env.EXPO_PUBLIC_FAKE_WIRE = "0";
  expect(() => wire()).toThrow(/adapter is not present|does not export a NativeWire/);
  process.env.EXPO_PUBLIC_FAKE_WIRE = "1";
});

test("the adapter module must export a NativeWire class", () => {
  expect(isNativeModule({ NativeWire: class {} })).toBe(true);
  expect(isNativeModule({ NativeWire: {} })).toBe(false);
  expect(isNativeModule({})).toBe(false);
  expect(isNativeModule(undefined)).toBe(false);
});

test("wirePaths creates custody and log directories under the document directory and hands the crate plain paths", () => {
  const p = wirePaths();
  expect(p.custodyDir).toBe("/data/user/0/com.stevengonsalvez.ainb.mobile/files/wire/custody");
  expect(p.logDir).toBe("/data/user/0/com.stevengonsalvez.ainb.mobile/files/wire/log");
  expect(wirePaths()).toBe(p); // cached
  expect(pathOf("file:///var/mobile/Containers/Data/Application/ABC/Documents/wire/custody/")).toBe(
    "/var/mobile/Containers/Data/Application/ABC/Documents/wire/custody",
  );
  expect(pathOf("file:///a/b%20c/")).toBe("/a/b c");
});
