# ainb mobile (M1)

Expo companion for a paired hangar daemon: hosts, sessions with live status,
transcript, prompt send, banner answers and (later) a terminal over the peer
socket.

Nothing here is linked by `ainb` or the daemon and the directory is outside
the Cargo workspace, so v1.29.0 behaviour is untouched. `src/wire/index.ts`
selects the transport: `EXPO_PUBLIC_FAKE_WIRE=1` is the in-memory FakeWire;
without it the app loads lane E's adapter (`src/wire/native.ts`, `NativeWire`
over the ubrn-linked `ainb-wire-mobile` crate) with the custody and log
directories from `wirePaths()` (expo-file-system, `Documents/ainb/custody`
and `Documents/ainb/log`; the custody directory must be marked excluded from
the iOS backup when the adapter goes live). A missing or malformed adapter refuses to start; the app never
falls back to the fake silently.

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
  the frozen proto records (docs/contracts/v2-next.md). The native binding
  (lane E's `MobileHost`, one object per host with a pull event loop) is
  wrapped by an adapter in `src/wire/` that presents this hostId-keyed,
  push-event interface; the screens never see the binding directly.
- `src/wire/fake.ts`: `FakeWire`, the in-memory transport for dev and tests.
- `src/terminal/`: xterm.js inside a locked-down webview. `postinstall`
  bundles the engine into `src/terminal/engine/bundle.generated.ts`
  (gitignored); with install scripts disabled run `npm run build:engine`
  before `npm run check` or `npm test`.
- `scripts/lint-wire.mjs`: fails `npm run check` if any file outside
  `src/wire/` parses wire JSON or names a snake_case wire field.
