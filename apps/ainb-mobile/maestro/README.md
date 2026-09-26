# Maestro flows

Every flow takes its ids from the environment; nothing FakeWire-specific is
in the YAML. Elements are addressed by `testID`, never by text, except the
copy the gate asserts ("Delivered", the question).

```sh
# FakeWire build on an emulator or simulator
EXPO_PUBLIC_FAKE_WIRE=1 npx expo run:android --variant release   # or run:ios
export FAKE="-e HOST_ID=01K5A0000000000000000AAAAA -e SESSION_KEY=claude:hangar -e SESSION_NAME=hangar -e ATTENTION_ID=att-1 -e QUESTION='Which runner do you mean?'"
maestro test $FAKE maestro/banner-answer.yaml
maestro test $FAKE maestro/background-foreground.yaml
maestro test $FAKE maestro/terminal-fixtures.yaml
maestro test $FAKE maestro/terminal-osc8-link.yaml

# real daemon (M1-14): the proof scenario supplies every value
maestro test -e OFFER="ainb://pair#..." -e HOST_ID=... -e ATTENTION_ID=... -e QUESTION="..." \
  -e NEW_ATTENTION_ID=... maestro/banner-answer.yaml
```
