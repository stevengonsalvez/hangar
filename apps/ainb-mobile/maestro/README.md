# Maestro flows

```sh
# FakeWire build on an emulator or simulator
EXPO_PUBLIC_FAKE_WIRE=1 npx expo run:android   # or run:ios
maestro test maestro/banner-answer.yaml
maestro test maestro/background-foreground.yaml
maestro test maestro/terminal-fixtures.yaml

# real daemon (M1-14): the proof scenario passes the offer
maestro test -e OFFER="ainb://pair#..." maestro/banner-answer.yaml
```

Every element is addressed by `testID`, never by text, except the copy the
gate asserts ("Delivered", the question).
