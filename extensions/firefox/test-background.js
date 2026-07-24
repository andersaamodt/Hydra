import assert from "node:assert/strict";
import fs from "node:fs";
import vm from "node:vm";

let listener;
let nativeRequests = 0;
const context = vm.createContext({
  URL,
  browser: {
    runtime: {
      onMessage: { addListener(value) { listener = value; } },
      async sendNativeMessage() {
        nativeRequests += 1;
        return { ok: true };
      },
    },
  },
});
vm.runInContext(fs.readFileSync(new URL("./background.js", import.meta.url), "utf8"), context);
assert.equal(typeof listener, "function");

const message = {
  protocol: "hydra-extension/v1",
  kind: "open_reddit",
  redditUrl: "https://www.reddit.com/r/science/",
};
assert.equal(
  (await listener(message, { tab: { url: "https://example.com/compromised" } })).ok,
  false,
);
assert.equal(nativeRequests, 0);
assert.equal(
  (await listener(message, { tab: { url: "https://www.reddit.com/r/science/" } })).ok,
  true,
);
assert.equal(nativeRequests, 1);
assert.equal(
  (await listener(
    { ...message, kind: "unsupported_action" },
    { tab: { url: "https://www.reddit.com/r/science/" } },
  )).ok,
  false,
);
assert.equal(nativeRequests, 1);
for (const redditUrl of [
  "https://attacker@www.reddit.com/r/science/",
  "https://www.reddit.com:444/r/science/",
  "https://example.com/r/science/",
]) {
  assert.equal(
    (await listener(
      { ...message, redditUrl },
      { tab: { url: "https://www.reddit.com/r/science/" } },
    )).ok,
    false,
  );
}
assert.equal(nativeRequests, 1);
assert.equal(
  (await listener(
    { ...message, kind: "steal_keys" },
    { tab: { url: "https://www.reddit.com/r/science/" } },
  )).ok,
  false,
);
assert.equal(nativeRequests, 1);
