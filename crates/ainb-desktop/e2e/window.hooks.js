// Mocha root hooks every spec runs under, loaded through `mochaOpts.require`
// in wdio.conf.js.
//
// They live here rather than in the config's `before` hook because wdio
// catches an error thrown from a config hook, logs it and runs the specs
// anyway: a precondition checked there cannot stop a spec. A root
// `beforeAll` that throws fails every test of the spec file under a
// `"before all" hook` failure that carries the message below, so the run says
// why it could not start instead of timing out somewhere downstream.

/** How long the window has to read visible once it has been given a frame. */
const VISIBLE_WITHIN_MS = 10_000;

/**
 * A visibility read that runs this long is a page that stopped answering. A
 * read that throws takes over a second to reject on the embedded driver, so
 * this sits well clear of that; one that answers takes milliseconds.
 */
const SLOW_READ_MS = 5_000;

/** The error for a page that stopped answering a read that ran `ms`. */
function hung(ms, cause) {
  return new Error(
    `the page did not answer a visibility read for ${(ms / 1000).toFixed(1)} s: ` +
      "the session or the webview has stopped responding",
    { cause },
  );
}

export const mochaHooks = {
  /**
   * Put the window on screen on macOS, and fail the spec when it will not go.
   *
   * The window the service launches there opens occluded: the page reads
   * `document.visibilityState === "hidden"`, and an occluded WKWebView never
   * runs requestAnimationFrame, so anything a spec waits on through a frame
   * (the review journey's in-page redraw timer, the terminal's repaint) never
   * arrives. Giving the window a frame brings it on screen within a second;
   * the wait makes that a fact before the spec runs. Linux under xvfb has no
   * occlusion, so it is left alone.
   */
  async beforeAll() {
    if (process.platform !== "darwin") return;
    await browser.setWindowRect(0, 30, 1280, 800);
    // What the reads did, kept by the poll itself. wdio's timer rejects at its
    // deadline without waiting for a read in flight, and waits out a first
    // read that hangs until the driver's own script timeout, so its rejection
    // alone cannot tell a hidden window from a page that stopped answering.
    let state = "unknown";
    let answered = false;
    let lastError = null;
    let lastReadMs = 0;
    let inFlight = null;
    try {
      await browser.waitUntil(
        async () => {
          const started = Date.now();
          const read = browser.execute(() => document.visibilityState);
          inFlight = { read, started };
          try {
            state = await read;
            answered = true;
            lastError = null;
          } catch (error) {
            lastError = error;
            throw error;
          } finally {
            lastReadMs = Date.now() - started;
            inFlight = null;
          }
          return state === "visible";
        },
        { timeout: VISIBLE_WITHIN_MS, interval: 250 },
      );
    } catch (error) {
      // A read still running at the deadline gets a grace to settle, and is
      // then judged by what it did rather than by when the timer fired.
      if (inFlight !== null) {
        const { read, started } = inFlight;
        let grace;
        const settled = await Promise.race([
          read.then(
            () => true,
            () => true,
          ),
          new Promise((resolve) => {
            grace = setTimeout(() => resolve(false), SLOW_READ_MS);
          }),
        ]);
        clearTimeout(grace);
        if (!settled) throw hung(Date.now() - started, error);
        if (lastError === null && state === "visible") {
          // The window made it, but only after the wait's deadline: worth a
          // line in the run's output, since a spec that follows may be tight.
          console.warn(
            `window.hooks: the window read visible only after the ${VISIBLE_WITHIN_MS / 1000} s wait ` +
              `(a read that started ${((Date.now() - started) / 1000).toFixed(1)} s ago answered late)`,
          );
          return;
        }
      }
      if (lastError !== null && lastReadMs >= SLOW_READ_MS) throw hung(lastReadMs, lastError);
      if (lastError !== null) {
        throw new Error(`the window's visibility could not be read: ${lastError.message ?? lastError}`, {
          cause: lastError,
        });
      }
      if (!answered) {
        throw new Error(`no visibility read completed in the ${VISIBLE_WITHIN_MS / 1000} s wait`, { cause: error });
      }
      throw new Error(
        `the window is still occluded ${VISIBLE_WITHIN_MS / 1000} s after it was placed on screen ` +
          `(document.visibilityState is "${state}"): an occluded WKWebView runs no animation frames, ` +
          "so this spec cannot run",
        { cause: error },
      );
    }
  },
};
