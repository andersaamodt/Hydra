"use strict";

const NATIVE_HOST = "org.hydra.desktop";
const ALLOWED_KINDS = new Set(["ping", "open_reddit"]);

function isRedditUrl(value) {
  try {
    const url = new URL(value);
    return (
      url.protocol === "https:" &&
      ["www.reddit.com", "old.reddit.com"].includes(url.hostname) &&
      !url.username &&
      !url.password &&
      (!url.port || url.port === "443")
    );
  } catch (_) {
    return false;
  }
}

browser.runtime.onMessage.addListener(async (message, sender) => {
  if (!sender.tab || !isRedditUrl(sender.tab.url) || !message || message.protocol !== "hydra-extension/v1") {
    return { ok: false, error: "untrusted extension message" };
  }
  if (!ALLOWED_KINDS.has(message.kind)) {
    return { ok: false, error: "unsupported extension message" };
  }
  if (!isRedditUrl(message.redditUrl)) {
    return { ok: false, error: "untrusted Reddit URL" };
  }
  const request = {
    protocol: "hydra-native-bridge/v1",
    kind: message.kind,
    redditUrl: message.redditUrl || sender.tab.url || ""
  };
  try {
    return await browser.runtime.sendNativeMessage(NATIVE_HOST, request);
  } catch (error) {
    void error;
    return {
      ok: false,
      error: "Hydra desktop is not available"
    };
  }
});
