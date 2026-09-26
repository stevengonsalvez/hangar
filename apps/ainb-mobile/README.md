# ainb mobile (M1)

Expo companion for a paired hangar daemon: hosts, sessions with live status,
transcript, prompt send, banner answers and (later) a terminal over the peer
socket.

Nothing here is linked by `ainb` or the daemon and the directory is outside
the Cargo workspace, so v1.29.0 behaviour is untouched. The native binding
(`ainb-wire-mobile`, lane E) is not linked yet: run with the fake transport.

```sh
npm ci
EXPO_PUBLIC_FAKE_WIRE=1 npx expo start
npm run check   # tsc + the C-M1-3 wire lint
npm test        # jest, always on FakeWire
```

## Layout

- `app/`: expo-router screens (Hosts, Pair, Sessions, Session with Transcript
  and Terminal tabs, Log).
- `src/wire/types.ts`: the `WireClient` interface the screens use. It mirrors
  the frozen proto records (docs/contracts/v2-next.md) and becomes a re-export
  of the ubrn bindings when they land.
- `src/wire/fake.ts`: `FakeWire`, the in-memory transport for dev and tests.
- `scripts/lint-wire.mjs`: fails `npm run check` if any file outside
  `src/wire/` parses wire JSON or names a snake_case wire field.
