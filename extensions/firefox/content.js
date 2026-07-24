"use strict";

const EDITOR_SELECTOR = "textarea, [contenteditable='true']";
const MARKER_PATTERN = /(?:Uncensorable record|Reddacted|Withdrawn from Reddit|discussion continues on Hydra)/i;
let scheduled = false;

function redditUrl(value) {
  try {
    const url = new URL(value, location.href);
    if (
      url.protocol !== "https:" ||
      !["www.reddit.com", "old.reddit.com"].includes(url.hostname) ||
      url.username ||
      url.password ||
      (url.port && url.port !== "443")
    ) return location.href;
    url.search = "";
    url.hash = "";
    return url.href;
  } catch (_) {
    return location.href;
  }
}

async function openHydra(url, kind = "open_reddit") {
  const response = await browser.runtime.sendMessage({
    protocol: "hydra-extension/v1",
    kind,
    redditUrl: redditUrl(url)
  });
  if (!response || !response.ok) {
    showNotice(response?.error || "Hydra desktop is not available");
  }
}

function showNotice(message) {
  let notice = document.querySelector(".hydra-notice");
  if (!notice) {
    notice = document.createElement("div");
    notice.className = "hydra-notice";
    notice.setAttribute("role", "status");
    document.body.append(notice);
  }
  notice.textContent = message;
  notice.hidden = false;
  window.setTimeout(() => { notice.hidden = true; }, 4000);
}

function addEditorControl(editor) {
  if (editor.dataset.hydraReady === "true") return;
  editor.dataset.hydraReady = "true";
  const button = document.createElement("button");
  button.type = "button";
  button.className = "hydra-editor-button";
  button.textContent = "Open in Hydra";
  button.title = "Continue this Reddit conversation in Hydra";
  button.addEventListener("click", () => openHydra(location.href));
  editor.insertAdjacentElement("afterend", button);
}

function compactMarker(link) {
  if (link.dataset.hydraCompacted === "true") return;
  const text = `${link.textContent || ""} ${link.parentElement?.textContent || ""}`;
  if (!MARKER_PATTERN.test(text)) return;
  link.dataset.hydraCompacted = "true";
  link.classList.add("hydra-record-link");
  link.textContent = "Hydra record";
  link.title = "Open the uncensorable record";
}

function scan() {
  document.querySelectorAll(EDITOR_SELECTOR).forEach(addEditorControl);
  document.querySelectorAll("a[href]").forEach(compactMarker);
}

function scheduleScan() {
  if (scheduled) return;
  scheduled = true;
  window.requestAnimationFrame(() => {
    scheduled = false;
    scan();
  });
}

new MutationObserver(scheduleScan).observe(document.documentElement, {
  childList: true,
  subtree: true
});
scan();
