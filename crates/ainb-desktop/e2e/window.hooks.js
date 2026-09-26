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
    let state = "unknown";
    try {
      await browser.waitUntil(
        async () => {
          state = await browser.execute(() => document.visibilityState);
          return state === "visible";
        },
        { timeout: VISIBLE_WITHIN_MS, interval: 250 },
      );
    } catch {
      throw new Error(
        `the window is still occluded ${VISIBLE_WITHIN_MS / 1000} s after it was placed on screen ` +
          `(document.visibilityState is "${state}"): an occluded WKWebView runs no animation frames, ` +
          "so this spec cannot run",
      );
    }
  },
};
