import {
  LENSES,
  JUDGMENT_GRACE_MS,
  activePersona,
  commentsFor,
  durabilityLabel,
  parseRedditObjectUrl,
  pendingJudgmentDecision,
  provenance,
  redditDepth,
  relativeTime,
  myFeedPosts,
  sortedPosts,
  validCommunity,
  visibleInlineText,
  whyShown,
} from "./model.js";

const invoke = window.__TAURI__?.core?.invoke;
const deepLink = window.__TAURI__?.deepLink;
const desktopDialog = window.__TAURI__?.dialog;
const session = {
  state: null,
  route: "feed",
  community: null,
  chamber: "hydra",
  lens: "new",
  audience: "all",
  selected: null,
  treeFilters: {},
  reddit: { community: null, items: [], rules: [], rulesAvailable: false, after: null, threadRoot: null, threadItems: [], focusedFullname: null, refreshTimer: null, refreshStep: 0, requestEpoch: 0 },
  openNostr: { items: [], loaded: false, filter: "all" },
  companions: { checked: false, bookClubInstalled: false },
  revealedBlocks: new Set(),
  confirmingReveals: new Set(),
  pendingJudgment: null,
  communityImages: new Map(),
  busy: false,
};

const view = document.querySelector("#view");
const contextPanel = document.querySelector("#context-panel");
const modalRoot = document.querySelector("#modal-root");
const toastRegion = document.querySelector("#toast-region");

function finishBoot() {
  const app = document.querySelector("#app");
  const splash = document.querySelector("#boot-splash");
  app.hidden = false;
  splash?.remove();
}

function formatBytes(size) {
  const bytes = Number(size) || 0;
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
}

function element(tag, options = {}, children = []) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(options)) {
    if (key === "class") node.className = value;
    else if (key === "text") node.textContent = value ?? "";
    else if (key === "dataset") Object.assign(node.dataset, value);
    else if (key.startsWith("on") && typeof value === "function") node.addEventListener(key.slice(2), value);
    else if (key.startsWith("aria-") && value !== null && value !== undefined) node.setAttribute(key, String(value));
    else if (value === true) node.setAttribute(key, "");
    else if (value !== false && value !== null && value !== undefined) node.setAttribute(key, String(value));
  }
  for (const child of children.flat()) {
    if (child === null || child === undefined) continue;
    node.append(child instanceof Node ? child : document.createTextNode(String(child)));
  }
  return node;
}

function actionButton(label, onClick, className = "quiet-button") {
  return element("button", { type: "button", class: className, text: label, disabled: session.busy, onclick: onClick });
}

async function copyText(value, success = "Copied.") {
  try {
    await navigator.clipboard.writeText(value);
    toast(success);
  } catch {
    window.prompt("Copy this text:", value);
  }
}

function setBusy(busy) {
  session.busy = busy;
  document.querySelector("#app").setAttribute("aria-busy", String(busy));
  document.querySelectorAll("button, input[type='submit']").forEach((control) => {
    control.disabled = busy;
  });
}

async function runtime(command, payload = {}) {
  if (!invoke) throw new Error("Open Hydra as the desktop application to use local data.");
  if (command === "state") return invoke("runtime_state");
  if (command === "status") return invoke("runtime_status");
  return invoke("runtime_action", { action: command, input: payload });
}

function extractState(result) {
  return result?.data?.snapshot?.data ?? result?.data ?? result?.snapshot?.data ?? result;
}

async function refresh() {
  if (session.busy) return;
  setBusy(true);
  try {
    const [result, companions] = await Promise.all([
      runtime("state"),
      session.companions.checked
        ? Promise.resolve(session.companions)
        : invoke("companion_status").catch(() => session.companions),
    ]);
    session.state = extractState(result);
    session.companions = { checked: true, bookClubInstalled: Boolean(companions.bookClubInstalled) };
    render();
    finishBoot();
  } catch (error) {
    renderUnavailable(error);
  } finally {
    setBusy(false);
  }
}

async function mutate(action, payload, success) {
  if (session.busy) return null;
  setBusy(true);
  try {
    const result = await runtime(action, payload);
    closeModal();
    toast(success);
    const snapshot = extractState(result);
    if (snapshot?.personas) session.state = snapshot;
    else session.state = extractState(await runtime("state"));
    render();
    return result;
  } catch (error) {
    toast(readableError(error), true);
    const surfaced = error instanceof Error ? error : new Error(readableError(error));
    surfaced.hydraSurfaced = true;
    throw surfaced;
  } finally {
    setBusy(false);
  }
}

function readableError(error) {
  const text = typeof error === "string" ? error : error?.message ?? String(error);
  if (/Reddit credential vault failed: (?:No matching credential found|credential not found|object was not found)/i.test(text)) {
    return "Link a Reddit account to this persona before using Reddit tools.";
  }
  if (/object shape is invalid for its kind/i.test(text)) {
    return "That entry is empty, too long, or contains unsupported characters that could disguise what it says.";
  }
  try {
    const parsed = JSON.parse(text);
    return parsed.error ?? text;
  } catch {
    return text.replace(/^Error:\s*/, "");
  }
}

function parseCommunities(value) {
  const requested = String(value).split(",").map((item) => item.trim()).filter(Boolean);
  const invalid = requested.filter((item) => !validCommunity(item));
  if (invalid.length) throw new Error(`Invalid community ${invalid[0]}. Use Reddit-compatible letters, numbers, or underscores.`);
  const communities = [...new Set(requested.map(validCommunity))];
  if (!communities.length) throw new Error("Add at least one valid /h/ community.");
  return communities;
}

function configuredCrosspostDefault(kind, community = null) {
  const settings = session.state?.settings ?? {};
  const persona = activePersona(session.state);
  let value = Boolean(settings.crosspost_default);
  const personaValue = settings.persona_crosspost_defaults?.[persona?.id];
  if (typeof personaValue === "boolean") value = personaValue;
  const contentValue = settings.content_crosspost_defaults?.[kind];
  if (typeof contentValue === "boolean") value = contentValue;
  const communityValue = community ? settings.community_crosspost_defaults?.[community] : undefined;
  if (typeof communityValue === "boolean") value = communityValue;
  return value;
}

function crosspostOverride(value) {
  if (value === true) return "on";
  if (value === false) return "off";
  return "inherit";
}

function applyOverride(map, key, value) {
  const next = { ...(map ?? {}) };
  if (value === "inherit") delete next[key];
  else next[key] = value === "on";
  return next;
}

function parseCommunityOverrides(value) {
  const result = {};
  for (const raw of String(value ?? "").split(/\r?\n/)) {
    const line = raw.trim();
    if (!line) continue;
    const [name, setting, ...extra] = line.split("=").map((item) => item.trim());
    const community = validCommunity(name);
    if (!community || extra.length || !["on", "off"].includes(setting)) {
      throw new Error(`Invalid community override “${line}”. Use science=on or science=off.`);
    }
    result[community] = setting === "on";
  }
  return result;
}

function toast(message, error = false) {
  const item = element("div", { class: `toast${error ? " error" : ""}`, text: message });
  toastRegion.append(item);
  window.setTimeout(() => item.remove(), 5200);
}

function setRoute(route, community = null) {
  if (route !== "community" || community !== session.community) stopRedditThreadRefresh();
  session.route = route;
  session.community = community;
  session.selected = null;
  document.querySelectorAll(".nav-item").forEach((item) => item.classList.toggle("is-active", item.dataset.nav === route));
  render();
}

async function openHydraLink(value) {
  try {
    if (typeof value !== "string" || value.length > 8192) throw new Error("Link is too large");
    const link = new URL(value);
    if (
      link.protocol !== "hydra:" ||
      link.username ||
      link.password ||
      link.port
    ) throw new Error("Unsupported link shape");
    if (link.hostname === "reddit") {
      const redditUrl = link.searchParams.get("url");
      const source = link.searchParams.get("source");
      if (source && source !== "open_reddit") {
        throw new Error("Unsupported Reddit link source");
      }
      const target = parseRedditObjectUrl(redditUrl);
      if (target) {
        await openRedditObject(target);
      } else {
        setRoute("reddit");
      }
      toast("Reddit object opened. Browsing data remains transient.");
      return;
    }
    if (link.hostname === "nostr") {
      const uri = link.searchParams.get("uri");
      if (!uri?.startsWith("nostr:")) throw new Error("Missing portable Nostr URI");
      const persona = activePersona(session.state);
      const resolved = await runtime("nostr.resolve", { persona_id: persona?.id ?? null, uri });
      session.openNostr.items = [resolved.result.item];
      session.openNostr.loaded = true;
      setRoute("open-nostr");
      toast("Verified Nostr event opened. It remains transient until saved or used.");
      return;
    }
    toast("Unsupported Hydra link destination.", true);
  } catch {
    toast("Invalid Hydra link.", true);
  }
}

async function openRedditObject(target) {
  const persona = activePersona(session.state);
  if (!persona?.redditLinked) {
    setRoute("reddit");
    toast("Link this persona’s Reddit account to open the live Reddit object.", true);
    return;
  }
  session.chamber = "reddit";
  setRoute("community", target.community);
  session.reddit.community = target.community;
  session.reddit.focusedFullname = target.commentFullname ?? target.postFullname;
  await loadRedditThread({
    fullname: target.postFullname,
    subreddit: target.community,
    permalink: `/r/${target.community}/comments/${target.postFullname.slice(3)}/`,
  });
  window.setTimeout(() => document.querySelector(`[data-reddit-fullname="${session.reddit.focusedFullname}"]`)?.scrollIntoView({ block: "center" }), 0);
}

async function listenForHydraLinks() {
  if (!deepLink) return;
  const current = await deepLink.getCurrent();
  await handleHydraLinks(current);
  await deepLink.onOpenUrl((links) => {
    void handleHydraLinks(links);
  });
}

async function handleHydraLinks(links) {
  for (const link of (links ?? []).slice(0, 16)) {
    await openHydraLink(link);
  }
}

function render() {
  document.documentElement.dataset.theme = session.state?.settings?.theme ?? "system";
  renderBrand();
  renderPersona();
  renderCommunities();
  renderContext();
  if (!activePersona(session.state)) renderWelcome();
  else if (session.selected) renderDiscussion(session.selected);
  else if (session.route === "messages") renderMessages();
  else if (session.route === "open-nostr") renderOpenNostr();
  else if (session.route === "reddit") renderRedditBridge();
  else if (session.route === "settings") renderSettings();
  else renderFeed();
  renderPendingJudgmentCallout();
}

function topicIdenticon(topic) {
  let hash = 2166136261;
  for (const character of topic) hash = Math.imul(hash ^ character.charCodeAt(0), 16777619) >>> 0;
  const hue = hash % 360;
  const cells = Array.from({ length: 15 }, (_, index) => ((hash >>> (index % 24)) & 1) ? index : null).filter((value) => value !== null);
  const mirrored = cells.flatMap((index) => {
    const row = Math.floor(index / 3);
    const column = index % 3;
    return column === 2 ? [[column, row]] : [[column, row], [4 - column, row]];
  });
  const squares = mirrored.map(([x, y]) => `<rect x="${x * 20}" y="${y * 20}" width="20" height="20"/>`).join("");
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100"><rect width="100" height="100" rx="18" fill="hsl(${hue} 32% 18%)"/><g fill="hsl(${(hue + 38) % 360} 72% 70%)">${squares}</g></svg>`;
  return `data:image/svg+xml,${encodeURIComponent(svg)}`;
}

function renderBrand() {
  const brand = document.querySelector(".brand");
  const mark = brand.querySelector(".brand-mark");
  const domain = brand.querySelector("small");
  const name = brand.querySelector("strong");
  const community = session.route === "community" ? session.community : null;
  if (community) {
    const appearance = session.state?.settings?.community_appearances?.[community];
    const verified = appearance && session.communityImages.get(`${community}:${appearance.sha256}`);
    mark.src = verified || topicIdenticon(community);
    mark.alt = appearance?.alt || `${community} community identicon`;
    domain.hidden = false;
    domain.textContent = "/h/";
    name.textContent = community;
    brand.setAttribute("aria-label", `${community} community`);
    if (appearance && !verified) void verifyCommunityImage(community, appearance);
  } else {
    mark.src = "hydra-icon.png";
    mark.alt = "";
    domain.hidden = true;
    name.textContent = "Hydra";
    brand.setAttribute("aria-label", "Hydra home");
  }
}

async function verifyCommunityImage(community, appearance) {
  const key = `${community}:${appearance.sha256}`;
  if (session.communityImages.has(key)) return;
  session.communityImages.set(key, null);
  try {
    const response = await fetch(appearance.url, { credentials: "omit", referrerPolicy: "no-referrer" });
    if (!response.ok || Number(response.headers.get("content-length") || 0) > 5 * 1024 * 1024) throw new Error("image unavailable");
    const reader = response.body.getReader();
    const chunks = [];
    let length = 0;
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      length += value.length;
      if (length > 5 * 1024 * 1024) { await reader.cancel(); throw new Error("image too large"); }
      chunks.push(value);
    }
    const bytes = new Uint8Array(length);
    let offset = 0;
    for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length; }
    const digest = [...new Uint8Array(await crypto.subtle.digest("SHA-256", bytes))].map((value) => value.toString(16).padStart(2, "0")).join("");
    if (digest !== appearance.sha256) throw new Error("image hash mismatch");
    const objectUrl = URL.createObjectURL(new Blob([bytes], { type: appearance.mime_type }));
    session.communityImages.set(key, objectUrl);
    if (session.route === "community" && session.community === community) renderBrand();
  } catch {
    session.communityImages.set(key, false);
  }
}

function renderPersona() {
  const persona = activePersona(session.state);
  const button = document.querySelector("#persona-button");
  button.querySelector(".avatar").textContent = persona?.displayName?.slice(0, 1).toUpperCase() || "?";
  button.querySelector("strong").textContent = persona?.displayName || "No persona";
  const detail = button.querySelector("small");
  detail.hidden = !persona;
  detail.textContent = persona ? `${persona.redditLinked ? "Reddit linked" : "Hydra only"} · ${persona.publicKey.slice(0, 10)}…` : "";
  const requests = session.state?.messageRequestCount ?? 0;
  const badge = document.querySelector("#message-badge");
  badge.hidden = requests === 0;
  badge.textContent = String(requests);
}

function subscribedCommunities() {
  const persona = activePersona(session.state);
  const subscriptions = (session.state?.subscriptions ?? []).filter((item) => item.personaId === persona?.id);
  const fromObjects = (session.state?.objects ?? []).flatMap((item) => item.communities ?? []);
  return [...new Set([...subscriptions.map((item) => item.community), ...fromObjects])].sort();
}

function renderCommunities() {
  const list = document.querySelector("#community-list");
  list.replaceChildren(...subscribedCommunities().map((community) => {
    const selected = session.community === community && session.route === "community";
    return element("button", {
      type: "button",
      class: `nav-item${selected ? " is-active" : ""}`,
      onclick: () => setRoute("community", community),
    }, [element("span", { text: "#" }), element("span", { text: `/h/${community}` })]);
  }));
}

function renderContext() {
  const state = session.state;
  const readiness = state?.readiness ?? [];
  const status = element("section", { class: "context-status" }, [
    element("h2", { text: "Status" }),
    ...readiness.map((item) => element("div", { class: "readiness-row" }, [
      element("span", { class: `status-dot ${item.state}` }),
      element("div", {}, [element("strong", { text: item.label }), element("p", { text: item.detail })]),
    ])),
  ]);
  contextPanel.replaceChildren(status);
}

function viewHeader(title, extras = []) {
  return element("header", { class: "view-header" }, [
    element("h1", { text: title }),
    ...extras,
  ]);
}

function chamberTabs() {
  const selectChamber = (chamber) => {
    if (chamber === "hydra") stopRedditThreadRefresh();
    session.chamber = chamber;
    renderFeed();
    window.setTimeout(() => document.querySelector(`.view-tabs [aria-selected="true"]`)?.focus(), 0);
  };
  const move = (event) => {
    if (!["ArrowLeft", "ArrowRight"].includes(event.key)) return;
    event.preventDefault();
    selectChamber(session.chamber === "hydra" ? "reddit" : "hydra");
  };
  return element("div", { class: "view-tabs", role: "tablist", "aria-label": "Community chamber" }, [
    element("button", {
      type: "button", role: "tab", class: `tab-button${session.chamber === "hydra" ? " is-active" : ""}`,
      "aria-selected": session.chamber === "hydra", tabindex: session.chamber === "hydra" ? "0" : "-1", text: `/h/${session.community}`,
      onclick: () => selectChamber("hydra"), onkeydown: move,
    }),
    element("button", {
      type: "button", role: "tab", class: `tab-button reddit${session.chamber === "reddit" ? " is-active" : ""}`,
      "aria-selected": session.chamber === "reddit", tabindex: session.chamber === "reddit" ? "0" : "-1", text: `/r/${session.community}`,
      onclick: () => selectChamber("reddit"), onkeydown: move,
    }),
  ]);
}

function lensBar() {
  return element("div", { class: "lens-bar", "aria-label": "Feed lens" }, LENSES.map(([id, label]) => element("button", {
    type: "button",
    class: `lens-button${session.lens === id ? " is-active" : ""}`,
    text: label,
    title: id === "controversial" ? "Orders by the smaller of positive and negative reaction counts" : `Use the ${label} lens`,
    onclick: () => { session.lens = id; renderFeed(); },
  })));
}

function audienceBar() {
  const audiences = [["all", "All personas"], ["reddit", "Reddit-linked"], ["followed", "Followed"]];
  return element("div", { class: "lens-bar", "aria-label": "Community audience" }, audiences.map(([id, label]) => element("button", {
    type: "button", class: `lens-button${session.audience === id ? " is-active" : ""}`, text: label,
    onclick: () => { session.audience = id; renderFeed(); },
  })));
}

function filterCommunityAudience(posts) {
  if (session.audience === "all") return posts;
  const persona = activePersona(session.state);
  const allowed = session.audience === "followed"
    ? new Set((session.state.follows ?? []).filter((item) => item.personaId === persona.id).map((item) => item.target))
    : new Set((session.state.personas ?? []).filter((item) => item.redditLinked).map((item) => item.publicKey));
  return posts.filter((post) => allowed.has(post.author));
}

function renderFeed() {
  const community = session.route === "community" ? session.community : null;
  const title = community ? `/${session.chamber === "reddit" ? "r" : "h"}/${community}` : session.route === "front" ? "Hydra Front Page" : session.route === "revisited" ? "Revisited" : "My Feed";
  const extras = community ? [chamberTabs()] : [];
  const header = viewHeader(title, extras);

  if (community && session.chamber === "reddit") {
    renderRedditCommunity(header, community);
    return;
  }

  let lens = session.lens;
  if (session.route === "revisited") lens = "revisited";
  let posts = sortedPosts(session.state, lens, community);
  if (community) posts = filterCommunityAudience(posts);
  if (!community && session.route === "feed") posts = myFeedPosts(session.state, posts);
  const list = element("div", { class: "content-list" });
  if (posts.length === 0) {
    list.append(emptyState(
      community ? `No posts in /h/${community}` : "No posts in My Feed",
      community ? "The Reddit tab may contain posts from the corresponding subreddit." : "Follow a persona or subscribe to a community to add posts.",
      "New post",
      () => showComposer(community),
    ));
  } else {
    list.append(...posts.map((post) => postCard(post, lens, community)));
  }
  const communityTools = community ? renderCommunityTools(community) : null;
  view.replaceChildren(...[header, communityTools, community ? audienceBar() : null, lensBar(), list].filter(Boolean));
}

function renderCommunityTools(community) {
  const persona = activePersona(session.state);
  const subscription = (session.state.subscriptions ?? []).find((item) => item.personaId === persona.id && item.community === community);
  const norms = (session.state.objects ?? []).filter((item) => item.kind === "norm" && item.communities?.includes(community));
  return element("section", { class: "community-tools", "aria-label": `Hydra community tools for ${community}` }, [
    element("div", { class: "community-actions" }, [
      actionButton(subscription ? "Unsubscribe" : "Subscribe privately", () => setCommunitySubscription(community, !subscription, false), subscription ? "quiet-button" : "primary-button"),
      actionButton(subscription?.public ? "Subscription is public" : "Publish subscription", () => setCommunitySubscription(community, true, !subscription?.public)),
      actionButton("Community image", () => showCommunityAppearanceEditor(community)),
      actionButton("Propose a norm", () => showNormComposer(community)),
    ]),
    element("details", { class: "norm-field" }, [
      element("summary", { text: `${norms.length} communal norm ${norms.length === 1 ? "statement" : "statements"}` }),
      element("p", { text: "Signed positions, not enforceable rules." }),
      ...norms.map((norm) => element("article", { class: "norm-card" }, [
        element("p", { text: norm.body }),
        element("div", { class: "post-actions" }, [
          actionButton(`Endorse · ${norm.currentScore ?? 0}`, () => react(norm.anchor, "+")),
          actionButton("Diverge", () => react(norm.anchor, "-")),
          actionButton("Reset", () => react(norm.anchor, "0")),
        ]),
      ])),
    ]),
  ]);
}

function showCommunityAppearanceEditor(community) {
  const current = session.state.settings?.community_appearances?.[community];
  const verified = current && session.communityImages.get(`${community}:${current.sha256}`);
  modal("Community image", `Choose how ${community} appears to you. The bare topic name does not change.`, element("div", {}, [
    element("div", { class: "community-image-preview" }, [
      element("img", { src: verified || topicIdenticon(community), alt: current?.alt || `${community} community identicon` }),
      element("div", {}, [element("small", { text: "/h/" }), element("strong", { text: community })]),
    ]),
    field("HTTPS image URL", "url", "url", current?.url ?? "", "Leave this empty to return to the deterministic topic identicon."),
    field("SHA-256", "text", "sha256", current?.sha256 ?? "", "The downloaded bytes must match this content hash."),
    field("MIME type", "select", "mime_type", current?.mime_type ?? "image/png", "", { values: [["image/png", "PNG"], ["image/jpeg", "JPEG"], ["image/webp", "WebP"]] }),
    field("Width", "number", "width", current?.width ?? 256, "Pixels; maximum 4096.", { min: 1, max: 4096 }),
    field("Height", "number", "height", current?.height ?? 256, "Pixels; maximum 4096.", { min: 1, max: 4096 }),
    field("Alt text", "text", "alt", current?.alt ?? `${community} community image`, "Describe the image rather than the community."),
    element("p", { class: "evidence-note", text: "Published appearance choices are signed, replaceable Flocking records; this local choice takes precedence in your view." }),
  ]), { submitLabel: current ? "Save local choice" : "Use image locally", onSubmit: (data) => {
    const appearances = { ...(session.state.settings?.community_appearances ?? {}) };
    const url = String(data.get("url") ?? "").trim();
    if (!url) delete appearances[community];
    else appearances[community] = {
      url,
      sha256: String(data.get("sha256") ?? "").trim().toLowerCase(),
      mime_type: String(data.get("mime_type")),
      width: Number(data.get("width")),
      height: Number(data.get("height")),
      alt: String(data.get("alt") ?? "").trim(),
    };
    return mutate("settings.update", { community_appearances: appearances }, url ? "Community image saved locally." : "Deterministic community identicon restored.");
  } });
}

function emptyState(title, body, action, onAction) {
  return element("div", { class: "empty-state" }, [
    element("h2", { text: title }),
    body ? element("p", { text: body }) : null,
    actionButton(action, onAction, "primary-button"),
  ]);
}

function postCard(post, lens, community) {
  const effect = judgmentEffect(post, community);
  const pendingHide = effect?.pending && effect.kind === "hide";
  if (effect && !pendingHide && !session.revealedBlocks.has(post.anchor)) {
    return blockedPlaceholder(post, "Post", effect);
  }
  const origin = provenance(post);
  const vote = element("div", { class: "vote-column", "aria-label": "Hydra vote" }, [
    element("button", { type: "button", class: "vote-button", text: "▲", title: "Upvote or reaffirm", onclick: () => react(post.anchor, "+") }),
    element("span", { class: "vote-score", text: String(post.currentScore ?? 0), title: "Current Hydra score: one stance per persona" }),
    element("button", { type: "button", class: "vote-button down", text: "▼", title: "Downvote or reaffirm", onclick: () => react(post.anchor, "-") }),
  ]);
  const communities = (post.communities ?? []).map((name) => element("button", {
    type: "button", class: "community-chip", text: `/h/${name}`, onclick: () => setRoute("community", name),
  }));
  const main = element("div", { class: "post-main" }, [
    element("div", { class: "meta-line" }, [
      element("span", { class: `provenance ${origin.tone}`, text: origin.label }),
      element("button", { type: "button", class: "text-action", text: `${post.author.slice(0, 12)}…`, onclick: () => showPersonaProfile(post.author) }),
      element("span", { text: `· ${relativeTime(post.editedAt)}` }),
      element("span", { class: "state-chip", text: durabilityLabel(post.durability) }),
      post.disowned ? element("span", { class: "state-chip", text: "Disowning requested" }) : null,
    ]),
    element("button", { type: "button", class: "post-title", text: post.title || "Untitled discussion", onclick: () => { session.selected = post.anchor; render(); } }),
    element("p", { class: "post-body", text: post.body }),
    element("div", { class: "status-row" }, communities),
    emojiReactionStrip(post),
    element("div", { class: "post-actions" }, [
      element("button", { type: "button", class: "text-action", text: `${post.discussionCount ?? 0} replies`, onclick: () => { session.selected = post.anchor; render(); } }),
      element("button", { type: "button", class: "text-action", text: "Revisit", onclick: () => showRevisit(post) }),
      element("button", { type: "button", class: "text-action", text: "Reset vote", onclick: () => react(post.anchor, "0") }),
      element("button", { type: "button", class: "text-action", text: "Vote views", onclick: () => showVoteViews(post) }),
      element("button", { type: "button", class: "text-action", text: "React…", onclick: () => showEmojiReaction(post) }),
      instantJudgmentButton("Hide", "hide", post.anchor, (event) => queueHide(event, post)),
      community ? instantJudgmentButton(`Remove from /h/${community}`, "removal", post.anchor, (event) => queueRemoval(event, post, community)) : null,
      element("button", { type: "button", class: "text-action", text: "Feed reason", title: whyShown(post, lens, community), onclick: () => toast(whyShown(post, lens, community)) }),
    ]),
  ]);
  return element("article", { class: `post-card${pendingHide ? " is-pending-hide" : ""}` }, [vote, main]);
}

function renderDiscussion(anchor) {
  const post = session.state.objects.find((item) => item.anchor === anchor);
  if (!post) { session.selected = null; renderFeed(); return; }
  const origin = provenance(post);
  const comments = commentsFor(session.state, anchor);
  const effect = judgmentEffect(post, session.community);
  const pendingHide = effect?.pending && effect.kind === "hide";
  if (effect && !pendingHide && !session.revealedBlocks.has(post.anchor)) {
    view.replaceChildren(element("button", { type: "button", class: "back-button", text: "← Back to feed", onclick: () => { session.selected = null; render(); } }), blockedPlaceholder(post, "Post", effect));
    return;
  }
  const article = element("article", { class: `discussion${pendingHide ? " is-pending-hide" : ""}` }, [
    element("button", { type: "button", class: "back-button", text: "← Back to feed", onclick: () => { session.selected = null; render(); } }),
    element("div", { class: "meta-line" }, [
      element("span", { class: `provenance ${origin.tone}`, text: origin.label }),
      element("button", { type: "button", class: "text-action", text: post.author, onclick: () => showPersonaProfile(post.author) }),
      element("span", { text: relativeTime(post.editedAt) }),
      element("span", { class: "state-chip", text: durabilityLabel(post.durability) }),
    ]),
    element("h1", { text: post.title || "Untitled discussion" }),
    element("div", { class: "discussion-body", text: post.body }),
    emojiReactionStrip(post),
    ...(post.media ?? []).map((media) => element("section", { class: "context-card" }, [
      element("strong", { text: `${media.mimeType} · ${formatBytes(media.size)}` }),
      element("p", { class: "evidence-note", text: [media.dimensions, media.durationSeconds ? `${media.durationSeconds}s` : null, `sha256 ${media.sha256.slice(0, 16)}…`].filter(Boolean).join(" · ") }),
      element("p", { class: "evidence-note", text: media.preservation === "published" ? "Preserved locally, uploaded by content hash, and described by a Nostr file-metadata event." : media.preservation === "media_only" ? "Preserved locally and uploaded, but Nostr metadata publication is incomplete." : "Preserved locally only; relay-independent local continuity exists, but remote media replication is incomplete." }),
    ])),
    element("div", { class: "discussion-toolbar" }, [
      actionButton("▲ Upvote", () => react(post.anchor, "+")),
      element("span", { class: "vote-score", text: String(post.currentScore ?? 0), title: "Current Hydra score: one stance per persona" }),
      actionButton("▼ Downvote", () => react(post.anchor, "-")),
      actionButton("Reset vote", () => react(post.anchor, "0")),
      actionButton("Vote views", () => showVoteViews(post)),
      actionButton("React…", () => showEmojiReaction(post)),
      instantJudgmentButton("Hide", "hide", post.anchor, (event) => queueHide(event, post), "quiet-button"),
      session.community ? instantJudgmentButton(`Remove from /h/${session.community}`, "removal", post.anchor, (event) => queueRemoval(event, post, session.community), "quiet-button") : null,
      actionButton("Reply", () => showReply(post), "primary-button"),
      actionButton("Revisit", () => showRevisit(post)),
      post.author === activePersona(session.state)?.publicKey ? actionButton("Preserve media", () => preserveMedia(post)) : null,
      post.author === activePersona(session.state)?.publicKey ? actionButton("Edit", () => showEdit(post)) : null,
      post.author === activePersona(session.state)?.publicKey && !post.disowned ? actionButton("Disown…", () => showDisown(post), "danger-button") : null,
      post.redditProjected ? actionButton("Continuity…", () => showContinuity(post)) : null,
    ]),
    element("h2", { text: comments.length ? `${comments.length} replies` : "No replies yet" }),
    ...comments.map((comment) => commentView(comment)),
  ]);
  view.replaceChildren(article);
}

function commentView(comment) {
  const effect = judgmentEffect(comment, session.community);
  const pendingHide = effect?.pending && effect.kind === "hide";
  if (effect && !pendingHide && !session.revealedBlocks.has(comment.anchor)) {
    return blockedPlaceholder(comment, "Comment", effect, `margin-left:${Math.min(comment.depth, 6) * 22}px`);
  }
  const persona = activePersona(session.state);
  const origin = provenance(comment);
  return element("article", { class: `comment${pendingHide ? " is-pending-hide" : ""}`, style: `margin-left:${Math.min(comment.depth, 6) * 22}px` }, [
    element("div", { class: "meta-line" }, [
      element("span", { class: `provenance ${origin.tone}`, text: origin.label }),
      element("button", { type: "button", class: "text-action", text: comment.author, onclick: () => showPersonaProfile(comment.author) }),
      element("span", { text: relativeTime(comment.editedAt) }),
      comment.disowned ? element("span", { class: "state-chip", text: "Disowning requested" }) : null,
    ]),
    element("div", { class: "comment-body", text: comment.body }),
    emojiReactionStrip(comment),
    element("div", { class: "post-actions" }, [
      element("button", { type: "button", class: "text-action", text: `▲ ${comment.currentScore ?? 0}`, onclick: () => react(comment.anchor, "+") }),
      element("button", { type: "button", class: "text-action", text: "▼", onclick: () => react(comment.anchor, "-") }),
      element("button", { type: "button", class: "text-action", text: "Reply", onclick: () => showReply(comment) }),
      element("button", { type: "button", class: "text-action", text: "Revisit", onclick: () => showRevisit(comment) }),
      element("button", { type: "button", class: "text-action", text: "Reset vote", onclick: () => react(comment.anchor, "0") }),
      element("button", { type: "button", class: "text-action", text: "Vote views", onclick: () => showVoteViews(comment) }),
      element("button", { type: "button", class: "text-action", text: "React…", onclick: () => showEmojiReaction(comment) }),
      instantJudgmentButton("Hide", "hide", comment.anchor, (event) => queueHide(event, comment)),
      session.community ? instantJudgmentButton(`Remove from /h/${session.community}`, "removal", comment.anchor, (event) => queueRemoval(event, comment, session.community)) : null,
      comment.author === persona?.publicKey ? element("button", { type: "button", class: "text-action", text: "Edit", onclick: () => showEdit(comment) }) : null,
      comment.author === persona?.publicKey && !comment.disowned ? element("button", { type: "button", class: "text-action danger-button", text: "Disown…", onclick: () => showDisown(comment) }) : null,
    ]),
  ]);
}

function judgmentEffect(object, community) {
  for (const [kind, persisted] of [
    ["block", (community && object.topicBlocks?.[community]) || object.block],
    ["silence", (community && object.topicSilences?.[community]) || object.silence],
    ["hide", (community && object.topicHides?.[community]) || object.hide],
    ["removal", community && object.topicRemovals?.[community]],
  ]) {
    const decision = pendingJudgmentDecision(session.pendingJudgment, kind, object, community);
    if (decision === "exclude") {
      return {
        kind,
        topic: kind === "removal" ? community : session.pendingJudgment.topic,
        inherited: false,
        pending: true,
        cutoff: session.pendingJudgment.cutoff,
        reason: session.pendingJudgment.payload.reason,
      };
    }
    if (decision !== "allow" && persisted) {
      return { ...persisted, kind, ...(kind === "removal" ? { topic: community } : {}) };
    }
  }
  return null;
}

function judgmentOrigin(event) {
  const rect = event?.currentTarget?.getBoundingClientRect?.();
  if (!rect) return { x: window.innerWidth / 2, top: window.innerHeight / 2, bottom: window.innerHeight / 2 };
  return { x: rect.left + rect.width / 2, top: rect.top, bottom: rect.bottom };
}

function queueJudgment(event, options) {
  if (session.pendingJudgment) {
    toast("Apply or undo the pending judgment first.");
    return;
  }
  const persona = activePersona(session.state);
  if (!persona) return;
  const topic = options.topic ?? null;
  const cutoff = Math.floor(Date.now() / 1000);
  const origin = judgmentOrigin(event);
  closeModal();
  session.pendingJudgment = {
    ...options,
    topic,
    cutoff,
    origin,
    remainingMs: JUDGMENT_GRACE_MS,
    deadline: Date.now() + JUDGMENT_GRACE_MS,
    paused: false,
    pauseAfter: Date.now() + 300,
    committing: false,
    timeout: null,
    ticker: null,
    callout: null,
    outsideListener: null,
    payload: {
      persona_id: persona.id,
      target: options.target,
      public: false,
      action: options.action,
      topic,
      reason: null,
      ...(options.kind === "block" ? { blocked: options.excludes } : {}),
      ...(options.kind === "silence" ? { silenced: options.excludes } : {}),
    },
  };
  session.revealedBlocks.delete(options.target);
  render();
  schedulePendingJudgment();
}

function pausePendingJudgment() {
  const pending = session.pendingJudgment;
  if (!pending || pending.paused || pending.committing) return;
  pending.remainingMs = Math.max(0, pending.deadline - Date.now());
  pending.paused = true;
  window.clearTimeout(pending.timeout);
  window.clearInterval(pending.ticker);
  pending.timeout = null;
  pending.ticker = null;
  updatePendingJudgmentStatus();
}

function resumePendingJudgment() {
  const pending = session.pendingJudgment;
  if (!pending || !pending.paused || pending.committing || pending.pointerInside || pending.focusInside) return;
  pending.paused = false;
  schedulePendingJudgment();
}

function dismissPendingJudgmentCallout() {
  const pending = session.pendingJudgment;
  if (!pending || pending.committing) return;
  pending.calloutDismissed = true;
  pending.pointerInside = false;
  pending.focusInside = false;
  pending.callout?.remove();
  pending.callout = null;
  pending.statusNode = null;
  renderPendingJudgmentCallout();
  resumePendingJudgment();
}

function reopenPendingJudgmentCallout(event, kind, target) {
  const pending = session.pendingJudgment;
  if (!pending || pending.kind !== kind || pending.target !== target) return false;
  if (pending.calloutDismissed) {
    pending.origin = judgmentOrigin(event);
    pending.calloutDismissed = false;
    renderPendingJudgmentCallout();
  }
  return true;
}

function instantJudgmentButton(label, kind, target, onAction, className = "text-action") {
  const reveal = (event) => reopenPendingJudgmentCallout(event, kind, target);
  const pendingOrigin = session.pendingJudgment?.kind === kind && session.pendingJudgment?.target === target;
  return element("button", {
    type: "button",
    class: `${className}${pendingOrigin ? " judgment-origin" : ""}`,
    text: label,
    disabled: session.busy,
    onpointerenter: reveal,
    onfocus: reveal,
    onclick: (event) => { if (!reveal(event)) onAction(event); },
  });
}

function schedulePendingJudgment() {
  const pending = session.pendingJudgment;
  if (!pending || pending.paused || pending.committing) return;
  window.clearTimeout(pending.timeout);
  window.clearInterval(pending.ticker);
  pending.deadline = Date.now() + pending.remainingMs;
  pending.timeout = window.setTimeout(() => void commitPendingJudgment(), pending.remainingMs);
  pending.ticker = window.setInterval(updatePendingJudgmentStatus, 250);
  updatePendingJudgmentStatus();
}

function updatePendingJudgmentStatus() {
  const pending = session.pendingJudgment;
  if (!pending) return;
  const seconds = Math.max(0, Math.ceil((pending.paused ? pending.remainingMs : pending.deadline - Date.now()) / 1000));
  const status = pending.committing
    ? (pending.payload.public ? "Publishing…" : "Saving…")
    : pending.paused
      ? "Timer paused while you decide."
      : `${pending.payload.public ? "Publishing" : "Saving privately"} in ${seconds}s.`;
  if (pending.statusNode) pending.statusNode.textContent = status;
  if (pending.recallNode) pending.recallNode.textContent = `${pending.pastLabel} · ${seconds}s`;
}

function undoPendingJudgment() {
  const pending = session.pendingJudgment;
  if (!pending || pending.committing) return;
  window.clearTimeout(pending.timeout);
  window.clearInterval(pending.ticker);
  if (pending.outsideListener) document.removeEventListener("pointerdown", pending.outsideListener, true);
  pending.callout?.remove();
  session.pendingJudgment = null;
  render();
}

async function commitPendingJudgment() {
  const pending = session.pendingJudgment;
  if (!pending || pending.committing) return;
  window.clearTimeout(pending.timeout);
  window.clearInterval(pending.ticker);
  pending.committing = true;
  updatePendingJudgmentStatus();
  try {
    await mutate(pending.command, pending.payload, pending.success);
  } catch (error) {
    if (!error?.hydraSurfaced) toast(readableError(error), true);
  } finally {
    if (session.pendingJudgment === pending) {
      if (pending.outsideListener) document.removeEventListener("pointerdown", pending.outsideListener, true);
      pending.callout?.remove();
      session.pendingJudgment = null;
      render();
    }
  }
}

function renderPendingJudgmentCallout() {
  const pending = session.pendingJudgment;
  if (pending?.outsideListener) {
    document.removeEventListener("pointerdown", pending.outsideListener, true);
    pending.outsideListener = null;
  }
  document.querySelector(".judgment-callout")?.remove();
  document.querySelector(".pending-judgment-recall")?.remove();
  if (!pending) return;
  pending.recallNode = null;
  if (pending.calloutDismissed) {
    const recall = element("button", {
      type: "button",
      class: "pending-judgment-recall",
      "aria-label": `Reopen ${pending.pastLabel.toLowerCase()} options`,
      onpointerenter: (event) => reopenPendingJudgmentCallout(event, pending.kind, pending.target),
      onfocus: (event) => reopenPendingJudgmentCallout(event, pending.kind, pending.target),
      onclick: (event) => reopenPendingJudgmentCallout(event, pending.kind, pending.target),
    });
    recall.style.left = `${Math.min(window.innerWidth - 120, Math.max(12, pending.origin.x - 52))}px`;
    recall.style.top = `${Math.min(window.innerHeight - 36, Math.max(12, pending.origin.top))}px`;
    document.body.append(recall);
    pending.recallNode = recall;
    updatePendingJudgmentStatus();
    return;
  }

  const status = element("p", { class: "judgment-callout-status", "aria-live": "polite" });
  const publish = element("input", {
    type: "checkbox",
    checked: pending.payload.public,
    disabled: pending.committing,
    onchange: (event) => {
      pending.payload.public = event.currentTarget.checked;
      updatePendingJudgmentStatus();
    },
  });
  const options = [
    element("label", { class: "judgment-callout-toggle" }, [publish, element("span", { text: "Publish for followers" })]),
  ];
  if (pending.scopeTopic) {
    const scope = element("select", {
      "aria-label": "Scope",
      disabled: pending.committing,
      onchange: (event) => {
        pending.topic = event.currentTarget.value || null;
        pending.payload.topic = pending.topic;
      },
    }, [
      element("option", { value: pending.scopeTopic, text: `/h/${pending.scopeTopic}`, selected: pending.topic === pending.scopeTopic }),
      element("option", { value: "", text: "Everywhere", selected: !pending.topic }),
    ]);
    options.unshift(element("label", { class: "judgment-callout-field" }, [element("span", { text: "Scope" }), scope]));
  }
  const reason = element("input", {
    type: "text",
    value: pending.payload.reason ?? "",
    placeholder: "Reason (optional)",
    "aria-label": "Reason",
    disabled: pending.committing,
    oninput: (event) => { pending.payload.reason = event.currentTarget.value || null; },
  });
  const callout = element("aside", {
    class: "judgment-callout",
    role: "dialog",
    "aria-label": `${pending.pastLabel} options`,
    onpointerenter: () => {
      pending.pointerInside = true;
      if (Date.now() >= pending.pauseAfter) pausePendingJudgment();
    },
    onpointermove: () => {
      if (Date.now() >= pending.pauseAfter) pausePendingJudgment();
    },
    onpointerleave: () => { pending.pointerInside = false; resumePendingJudgment(); },
    onfocusin: () => { pending.focusInside = true; pausePendingJudgment(); },
    onfocusout: () => window.setTimeout(() => {
      pending.focusInside = Boolean(pending.callout?.contains(document.activeElement));
      resumePendingJudgment();
    }, 0),
    onpointerdown: pausePendingJudgment,
  }, [
    element("strong", { text: pending.pastLabel }),
    status,
    ...options,
    reason,
    element("div", { class: "judgment-callout-actions" }, [
      actionButton("Undo", undoPendingJudgment),
      actionButton("Apply now", () => void commitPendingJudgment(), "primary-button"),
    ]),
  ]);
  document.body.append(callout);
  pending.callout = callout;
  pending.statusNode = status;
  const margin = 12;
  const box = callout.getBoundingClientRect();
  const left = Math.min(window.innerWidth - box.width - margin, Math.max(margin, pending.origin.x - box.width / 2));
  const fitsBelow = pending.origin.bottom + box.height + 18 < window.innerHeight;
  callout.classList.toggle("is-above", !fitsBelow);
  callout.style.left = `${left}px`;
  callout.style.top = `${fitsBelow ? pending.origin.bottom + 12 : Math.max(margin, pending.origin.top - box.height - 12)}px`;
  callout.style.setProperty("--callout-pointer-x", `${Math.min(box.width - 22, Math.max(22, pending.origin.x - left))}px`);
  updatePendingJudgmentStatus();
  window.setTimeout(() => {
    const closeOnOutsidePress = (event) => {
      if (session.pendingJudgment !== pending) {
        document.removeEventListener("pointerdown", closeOnOutsidePress, true);
        pending.outsideListener = null;
        return;
      }
      if (pending.callout?.contains(event.target)) return;
      dismissPendingJudgmentCallout();
      document.removeEventListener("pointerdown", closeOnOutsidePress, true);
      pending.outsideListener = null;
    };
    pending.outsideListener = closeOnOutsidePress;
    document.addEventListener("pointerdown", closeOnOutsidePress, true);
  }, 0);
}

function queueHide(event, object) {
  const currentTopic = session.route === "community" && session.community ? session.community : null;
  queueJudgment(event, { kind: "hide", command: "hide.set", targetType: "anchor", target: object.anchor, excludes: true, action: "hide", topic: currentTopic, scopeTopic: currentTopic, pastLabel: "Hidden", success: "Item hidden from this persona’s view." });
}

function queueRemoval(event, object, topic) {
  queueJudgment(event, { kind: "removal", command: "membership.set", targetType: "anchor", target: object.anchor, excludes: true, action: "remove", topic, pastLabel: `Removed from /h/${topic}`, success: `Item removed from /h/${topic} in this persona’s view.` });
}

function queueBlock(event, target) {
  const currentTopic = session.route === "community" && session.community ? session.community : null;
  queueJudgment(event, { kind: "block", command: "block.set", targetType: "author", target, excludes: true, action: "block", topic: currentTopic, scopeTopic: currentTopic, pastLabel: "Blocked", success: "Block applied to this persona’s view." });
}

function queueSilence(event, target) {
  const currentTopic = session.route === "community" && session.community ? session.community : null;
  queueJudgment(event, { kind: "silence", command: "silence.set", targetType: "author", target, excludes: true, action: "silence", topic: currentTopic, scopeTopic: currentTopic, pastLabel: "Silenced from now on", success: "New activity from this persona is now silenced." });
}

function queueReverseJudgment(event, kind, target, effect) {
  const topic = effect.scope?.startsWith("topic:") ? effect.scope.slice(6) : effect.topic ?? null;
  const settings = {
    block: ["block.set", "author", "unblock", "Unblocked", "Direct unblock applied."],
    silence: ["silence.set", "author", "unsilence", "Unsilenced", "Direct unsilence applied."],
    hide: ["hide.set", "anchor", "unhide", "Unhidden", "Direct unhide applied."],
    removal: ["membership.set", "anchor", "restore", `Restored to /h/${topic}`, `Direct restoration applied in /h/${topic}.`],
  }[kind];
  queueJudgment(event, { kind, command: settings[0], targetType: settings[1], target, excludes: false, action: settings[2], topic, pastLabel: settings[3], success: settings[4] });
}

function blockedPlaceholder(object, label, effect, style = "") {
  const confirming = session.confirmingReveals.has(object.anchor);
  const silenced = effect.kind === "silence";
  const hidden = effect.kind === "hide";
  const removed = effect.kind === "removal";
  const cutoff = silenced && effect.cutoff ? `New activity since ${new Date(effect.cutoff * 1000).toLocaleString()} is silenced.` : null;
  const timing = effect.localTimingEvidence ? "Its signed time predates the silence, but Hydra first observed it afterward." : null;
  const explanation = [effect.why, cutoff, timing, effect.reason ? `Reason: ${effect.reason}` : null, effect.uncertain ? "Source data is incomplete, so Hydra is hiding this conservatively." : null].filter(Boolean).join(" ");
  const placeholderLabel = silenced
      ? `Silenced ${label.toLowerCase()}`
      : hidden
        ? `Hidden ${label.toLowerCase()}`
        : removed
          ? `${label} removed from /h/${effect.topic}`
          : `${label} from blocked user`;
  const target = silenced || (!hidden && !removed) ? object.author : object.anchor;
  return element("article", { class: "blocked-placeholder", style }, [
    effect.pending
      ? instantJudgmentButton(placeholderLabel, effect.kind, target, () => {}, "pending-judgment-label")
      : element("strong", { text: placeholderLabel }),
    confirming
      ? element("p", { text: `${label} is from ${object.author}. Reveal now?` })
      : element("p", { text: silenced
        ? (effect.inherited ? "Hidden through a silence judgment you follow." : "Hidden by your silence judgment.")
        : hidden
          ? (effect.inherited ? "Hidden through a content judgment you follow." : "Hidden by your content judgment.")
          : removed
            ? (effect.inherited ? "Removed through a community judgment you follow." : "Removed by your community judgment.")
            : (effect.inherited ? "Hidden through a block judgment you follow." : "Hidden by your block judgment.") }),
    element("div", { class: "post-actions" }, confirming
      ? [
          actionButton("Yes", () => { session.confirmingReveals.delete(object.anchor); session.revealedBlocks.add(object.anchor); render(); }),
          actionButton("No", () => { session.confirmingReveals.delete(object.anchor); render(); }),
        ]
      : [
          actionButton("Reveal anyway", () => { session.confirmingReveals.add(object.anchor); render(); }),
          explanation ? actionButton("Why?", () => toast(explanation)) : null,
          effect.pending
            ? actionButton("Undo", undoPendingJudgment)
            : instantJudgmentButton(
                silenced ? "Unsilence" : hidden ? "Unhide" : removed ? "Restore" : "Unblock",
                effect.kind,
                target,
                (event) => queueReverseJudgment(event, effect.kind, target, effect),
                "quiet-button",
              ),
        ]),
  ]);
}

function renderRedditCommunity(header, community) {
  const persona = activePersona(session.state);
  const cached = session.reddit.community === community ? session.reddit.items : [];
  if (persona.redditLinked && (cached.length || session.reddit.community === community)) {
    const toolbar = element("div", { class: "community-actions" }, [
      actionButton("Refresh Reddit", () => loadRedditCommunity(community)),
      actionButton("Leave thread", () => { stopRedditThreadRefresh(); session.reddit.threadRoot = null; session.reddit.threadItems = []; renderFeed(); }),
    ]);
    const items = session.reddit.threadRoot ? session.reddit.threadItems : cached;
    const label = session.reddit.threadRoot ? "Merged live thread" : `Live /r/${community}`;
    const list = element("div", { class: "content-list", "aria-label": label }, items.length
      ? items.map((item) => redditCard(item, community, session.reddit.threadRoot ? redditDepth(item, items) : 0))
      : [element("p", { class: "evidence-note", text: "Reddit returned no posts for this view." })]);
    const rules = element("details", { class: "norm-field reddit-rules" }, [
      element("summary", { text: session.reddit.rulesAvailable ? `${session.reddit.rules.length} centralized Reddit rule${session.reddit.rules.length === 1 ? "" : "s"}` : "Centralized Reddit rules unavailable" }),
      element("p", { class: "evidence-note", text: "These rules are imposed and enforced by Reddit’s subreddit operators. They do not govern /h/." }),
      ...(!session.reddit.rulesAvailable
        ? [element("p", { text: "Hydra could not retrieve Reddit’s current rule list. This does not mean the subreddit has no rules." })]
        : session.reddit.rules.length
        ? session.reddit.rules.map((rule) => element("article", { class: "norm-card" }, [
            element("strong", { text: rule.title }),
            rule.description ? element("p", { text: rule.description }) : null,
          ]))
        : [element("p", { text: "Reddit supplied no rules for this community." })]),
    ]);
    view.replaceChildren(header, toolbar, rules, list);
    return;
  }
  const body = element("div", { class: "content-list" }, [
    emptyState(
      persona.redditLinked ? `Browse /r/${community}` : "Connect Reddit",
      persona.redditLinked ? "" : "Linking adds an optional Reddit projection endpoint.",
      persona.redditLinked ? "Load Reddit" : "Open Reddit Bridge",
      persona.redditLinked ? () => loadRedditCommunity(community) : () => setRoute("reddit"),
    ),
  ]);
  view.replaceChildren(header, body);
}

async function loadRedditCommunity(community) {
  const persona = activePersona(session.state);
  const epoch = ++session.reddit.requestEpoch;
  try {
    const result = await runtime("reddit.browse.community", { persona_id: persona.id, subreddit: community, sort: "hot", after: null });
    if (epoch !== session.reddit.requestEpoch || session.route !== "community" || session.community !== community || session.chamber !== "reddit") return;
    session.reddit = {
      community,
      items: result.result?.items ?? [],
      rules: result.result?.rules ?? [],
      rulesAvailable: result.result?.rulesAvailable === true,
      after: result.result?.after ?? null,
      threadRoot: null,
      threadItems: [],
      focusedFullname: null,
      refreshTimer: null,
      refreshStep: 0,
      requestEpoch: epoch,
    };
    toast(`Loaded /r/${community} transiently. Nothing was published to Nostr.`);
    renderFeed();
  } catch (error) {
    if (epoch === session.reddit.requestEpoch) toast(readableError(error), true);
  }
}

function redditUrl(item) {
  const value = item.permalink || "";
  return value.startsWith("http") ? value : `https://www.reddit.com${value.startsWith("/") ? "" : "/"}${value}`;
}

function redditCard(item, community, depth = 0) {
  const persona = activePersona(session.state);
  const unavailable = item.removed || item.deleted;
  const state = item.removed ? "Removed" : item.deleted ? "Deleted" : item.locked ? "Locked" : item.edited_at ? "Edited" : "Live on Reddit";
  const isPost = String(item.fullname).startsWith("t3_");
  const mergedReplies = hydraRepliesForExternal(redditUrl(item));
  return element("article", { class: `post-card reddit-card${session.reddit.focusedFullname === item.fullname ? " is-focused" : ""}`, "data-reddit-fullname": item.fullname, style: `margin-left:${Math.min(Math.max(depth, 0), 6) * 22}px` }, [
    element("div", { class: "post-main" }, [
      element("div", { class: "meta-line" }, [
        element("span", { class: "provenance reddit", text: `Reddit · /r/${item.subreddit || community}` }),
        element("span", { text: item.author || "[deleted]" }),
        element("span", { class: "state-chip", text: state }),
        element("span", { text: relativeTime(item.created_at) }),
      ]),
      isPost ? element("button", { type: "button", class: "post-title", text: visibleInlineText(item.title || "Untitled Reddit post"), onclick: () => loadRedditThread(item) }) : null,
      element("p", { class: "post-body", text: item.body || (unavailable ? "Reddit no longer supplies this text." : "") }),
      element("div", { class: "post-actions" }, [
        isPost ? actionButton("Open thread", () => loadRedditThread(item)) : null,
        actionButton("Reply in Hydra", () => showRedditReply(item), "primary-button"),
      ]),
      element("p", { class: "evidence-note", text: "This Reddit-supplied body is transient and is not published to Nostr." }),
      ...mergedReplies.map((reply) => commentView(reply)),
    ]),
  ]);
}

function hydraRepliesForExternal(url) {
  const maximumItems = 2000;
  const maximumDepth = 64;
  const objects = session.state.objects ?? [];
  const output = [];
  const seen = new Set();
  const pending = objects
    .filter((candidate) => candidate.externalParent === url)
    .reverse()
    .map((item) => ({ item, depth: 1 }));
  while (pending.length && output.length < maximumItems) {
    const { item, depth } = pending.pop();
    if (seen.has(item.anchor)) continue;
    seen.add(item.anchor);
    output.push({ ...item, depth });
    if (depth >= maximumDepth) continue;
    const children = objects.filter((candidate) => candidate.parent === item.anchor);
    for (let index = children.length - 1; index >= 0; index -= 1) {
      pending.push({ item: children[index], depth: depth + 1 });
    }
  }
  return output;
}

async function loadRedditThread(post) {
  const persona = activePersona(session.state);
  const epoch = ++session.reddit.requestEpoch;
  const community = session.community;
  try {
    const result = await runtime("reddit.browse.thread", { persona_id: persona.id, post: post.fullname });
    if (epoch !== session.reddit.requestEpoch || session.route !== "community" || session.community !== community || session.chamber !== "reddit") return;
    session.reddit.threadRoot = post.fullname;
    session.reddit.threadItems = result.result?.items ?? [post];
    resetRedditThreadRefresh();
    toast("Current Reddit thread loaded transiently. Hydra-only replies remain linked by their external parent.");
    renderFeed();
  } catch (error) {
    if (epoch === session.reddit.requestEpoch) toast(readableError(error), true);
  }
}

function stopRedditThreadRefresh() {
  if (session.reddit.refreshTimer) window.clearTimeout(session.reddit.refreshTimer);
  session.reddit.refreshTimer = null;
  session.reddit.refreshStep = 0;
  session.reddit.requestEpoch += 1;
}

function resetRedditThreadRefresh() {
  stopRedditThreadRefresh();
  scheduleRedditThreadRefresh();
}

function scheduleRedditThreadRefresh() {
  const root = session.reddit.threadRoot;
  const epoch = session.reddit.requestEpoch;
  if (!root) return;
  const intervals = [15, 30, 60, 120, 300];
  const delay = intervals[Math.min(session.reddit.refreshStep, intervals.length - 1)] * 1000;
  session.reddit.refreshTimer = window.setTimeout(async () => {
    if (session.reddit.threadRoot !== root) return;
    if (session.busy || modalRoot.childElementCount || document.hidden) {
      scheduleRedditThreadRefresh();
      return;
    }
    try {
      const persona = activePersona(session.state);
      const result = await runtime("reddit.browse.thread", { persona_id: persona.id, post: root });
      if (session.reddit.threadRoot !== root || session.reddit.requestEpoch !== epoch) return;
      session.reddit.threadItems = result.result?.items ?? session.reddit.threadItems;
      session.reddit.refreshStep += 1;
      renderFeed();
    } catch (error) {
      toast(`Reddit refresh paused: ${readableError(error)}`, true);
    }
    scheduleRedditThreadRefresh();
  }, delay);
}

function showRedditReply(item) {
  const persona = activePersona(session.state);
  modal("Reply from Hydra", "The reply is stored in Hydra. A Reddit projection is optional.", element("div", {}, [
    field("Reply", "textarea", "body", "", "The reply is stored in Hydra even when Reddit projection is unavailable.", { required: true }),
    toggle("Also project this reply to Reddit", "crosspost", configuredCrosspostDefault("comment", validCommunity(item.subreddit || session.community)), `As u/${persona.redditUsername || "linked account"} to exact Reddit target ${item.fullname}. This publicly links the accounts.`),
  ]), { submitLabel: "Post Hydra reply", onSubmit: async (data) => {
    const rootItem = session.reddit.threadItems.find((entry) => entry.fullname === session.reddit.threadRoot) || item;
    const created = await runtime("comment.create_external", {
      persona_id: persona.id,
      root_url: redditUrl(rootItem),
      parent_url: redditUrl(item),
      communities: [validCommunity(item.subreddit || session.community)],
      body: data.get("body"),
    });
    if (data.get("crosspost")) {
      const queued = await runtime("reddit.comment.queue", { persona_id: persona.id, anchor: created.result.anchor, parent: item.fullname, attribution: null, link: null });
      await runtime("reddit.projection.execute", { projection_id: queued.result.projectionId });
    }
    closeModal();
    session.state = extractState(await runtime("state"));
    resetRedditThreadRefresh();
    toast(data.get("crosspost") ? "Reply saved in Hydra and projected to Reddit." : "Reply saved in Hydra only.");
    renderFeed();
  } });
}

function renderMessages() {
  const persona = activePersona(session.state);
  const messages = (session.state.messages ?? []).filter((item) => item.personaId === persona.id);
  const header = viewHeader("Messages");
  const body = element("div", { class: "content-list" }, [
    messages.length ? actionButton("New message", showMessageComposer, "primary-button") : null,
    ...(messages.length ? messages.map((message) => element("article", { class: "context-card" }, [
      element("div", { class: "meta-line" }, [element("strong", { text: message.peer }), element("span", { text: relativeTime(message.createdAt) }), message.request ? element("span", { class: "state-chip", text: "Message request" }) : null]),
      element("p", { text: message.body }),
      element("div", { class: "post-actions" }, [
        actionButton("Reply as this persona", () => showMessageComposerTo(message.peer), "primary-button"),
      ]),
    ])) : [emptyState("No messages", "This inbox belongs only to the selected persona.", "Write a message", showMessageComposer)]),
  ]);
  view.replaceChildren(header, body);
}

function renderOpenNostr() {
  const header = viewHeader("Open Nostr");
  const controls = element("div", { class: "community-actions" }, [
    actionButton("Refresh from relays", loadOpenNostr, "primary-button"),
  ]);
  const list = element("div", { class: "content-list" });
  if (!session.openNostr.loaded) {
    list.append(emptyState("No relay sample loaded", "Reading remains transient until you curate or categorize an event.", "Load from relays", loadOpenNostr));
  } else if (!session.openNostr.items.length) {
    list.append(emptyState("No recent discussion returned", "Try again later or choose different read relays in Settings.", "Refresh", loadOpenNostr));
  } else {
    const items = filteredOpenNostrItems();
    if (items.length) list.append(...items.map(openNostrCard));
    else list.append(emptyState("Nothing in this view", "The current relay sample contains no events matching this category.", "Show everything", () => {
      session.openNostr.filter = "all";
      renderOpenNostr();
    }));
  }
  const surfaces = [header];
  if (session.openNostr.loaded) surfaces.push(controls);
  if (session.openNostr.items.length) surfaces.push(openNostrFilterBar());
  surfaces.push(list);
  view.replaceChildren(...surfaces);
}

function filteredOpenNostrItems() {
  if (session.openNostr.filter === "tagged") {
    return session.openNostr.items.filter((item) => item.topics?.length);
  }
  if (session.openNostr.filter === "uncategorized") {
    return session.openNostr.items.filter((item) => !item.topics?.length);
  }
  return session.openNostr.items;
}

function openNostrFilterBar() {
  const filters = [["all", "All"], ["tagged", "Tagged"], ["uncategorized", "Uncategorized"]];
  return element("div", { class: "lens-bar", "aria-label": "Open Nostr view" }, filters.map(([id, label]) => element("button", {
    type: "button",
    class: `lens-button${session.openNostr.filter === id ? " is-active" : ""}`,
    text: label,
    onclick: () => {
      session.openNostr.filter = id;
      renderOpenNostr();
    },
  })));
}

function openNostrCard(item) {
  const topics = item.topics?.length ? item.topics : [];
  if (item.canon) return canonNostrCard(item);
  return element("article", { class: "post-card open-nostr-card" }, [
    element("div", { class: "post-main" }, [
      element("div", { class: "meta-line" }, [
        element("span", { class: "provenance native", text: "Nostr" }),
        element("span", { text: `${String(item.author).slice(0, 14)}…` }),
        element("span", { text: relativeTime(item.createdAt) }),
        element("span", { class: "state-chip", text: topics.length ? topics.map((topic) => `#${topic}`).join(" · ") : "Uncategorized" }),
      ]),
      element("p", { class: "post-body", text: item.body || "This event has no text body." }),
      element("div", { class: "post-actions" }, [
        actionButton("Categorize locally", () => showNostrCategorize(item)),
        actionButton("Share to /h/", () => showNostrCuration(item), "primary-button"),
      ]),
    ]),
  ]);
}

function canonNostrCard(item) {
  const record = item.canon;
  const creatorLine = record.creators?.length ? record.creators.join(", ") : `${String(item.author).slice(0, 14)}…`;
  return element("article", { class: "post-card open-nostr-card canon-record" }, [
    element("div", { class: "post-main" }, [
      element("div", { class: "meta-line" }, [
        element("span", { class: "provenance native", text: "Canon" }),
        element("span", { class: "state-chip", text: String(record.role).replaceAll("-", " ") }),
        element("span", { text: relativeTime(item.createdAt) }),
      ]),
      element("h2", { text: record.title }),
      element("p", { class: "evidence-note", text: creatorLine }),
      record.summary ? element("p", { class: "post-body", text: record.summary }) : null,
      record.identifiers?.length ? element("p", { class: "evidence-note", text: record.identifiers.join(" · ") }) : null,
      element("div", { class: "post-actions" }, [
        actionButton("Keep locally", () => keepNostrEvent(item), "primary-button"),
        record.role === "work" && record.identifiers?.length
          ? actionButton("Discuss in Hydra", () => showCanonDiscussion(item))
          : null,
        bookClubCrossLinksAvailable() && item.bookClubUrl
          ? actionButton("Open in Book Club", () => openBookClub(item.bookClubUrl))
          : null,
        item.portable ? actionButton("Copy Nostr link", () => copyPortableLink(item.portable)) : null,
      ]),
    ]),
  ]);
}

function bookClubCrossLinksAvailable() {
  return session.companions.bookClubInstalled
    && session.state?.settings?.cross_links?.book_club_enabled !== false;
}

function showCanonDiscussion(item) {
  const identifier = item.canon?.identifiers?.find((value) => typeof value === "string" && value.includes(":"));
  const separator = identifier?.indexOf(":") ?? -1;
  if (separator < 1) {
    toast("This record has no standard work identifier to anchor a shared thread.", true);
    return;
  }
  const system = identifier.slice(0, separator);
  const persona = activePersona(session.state);
  modal("Discuss this work", `This publishes a standard NIP-22 comment rooted at ${identifier}, so Book Club and other Nostr clients can find the same thread.`, element("div", {}, [
    field("Community", "input", "community", validCommunity(session.community || "books"), "One Hydra community label is required.", { required: true }),
    field("Comment", "textarea", "body", "", "Public on your configured write relays.", { required: true }),
  ]), { submitLabel: "Publish comment", onSubmit: async (data) => {
    await runtime("comment.create_external", {
      persona_id: persona.id,
      root_system: system,
      root_id: identifier,
      parent_system: system,
      parent_id: identifier,
      communities: [validCommunity(data.get("community"))],
      body: data.get("body"),
    });
    closeModal();
    session.state = extractState(await runtime("state"));
    toast("Published to the work’s shared Nostr discussion.");
    renderFeed();
  } });
}

async function keepNostrEvent(item) {
  try {
    await runtime("nostr.keep", { event_json: item.event });
    toast("Kept the verified Canon event in Hydra’s local evidence.");
  } catch (error) {
    toast(readableError(error), true);
  }
}

function openBookClub(url) {
  if (typeof url !== "string" || !url.startsWith("bookclub://nostr/")) {
    toast("Invalid Book Club handoff.", true);
    return;
  }
  window.location.assign(url);
}

async function copyPortableLink(uri) {
  try {
    await navigator.clipboard.writeText(uri);
    toast("Copied portable Nostr link.");
  } catch {
    toast("Could not copy the portable link.", true);
  }
}

async function loadOpenNostr() {
  const persona = activePersona(session.state);
  try {
    const response = await runtime("nostr.open", { persona_id: persona?.id ?? null, since: null, limit: 30 });
    session.openNostr.items = response.result?.items ?? [];
    session.openNostr.loaded = true;
    renderOpenNostr();
    toast(`Loaded ${session.openNostr.items.length} recent Nostr event${session.openNostr.items.length === 1 ? "" : "s"}.`);
  } catch (error) {
    toast(readableError(error), true);
  }
}

function showNostrCategorize(item) {
  const persona = activePersona(session.state);
  modal("Categorize locally", "This private assignment changes only the selected persona’s local view.", field("Hydra topics", "text", "communities", item.topics?.join(", ") ?? "", "Separate /h/ topic names with commas.", { required: true, placeholder: "science, biology" }), {
    submitLabel: "Save locally",
    onSubmit: (data) => mutate("nostr.categorize_local", { persona_id: persona.id, event_json: item.event, communities: parseCommunities(data.get("communities")) }, "Private topic assignment saved."),
  });
}

function showNostrCuration(item) {
  const persona = activePersona(session.state);
  modal("Share to Hydra topics", "Hydra publishes a standard Nostr repost with topic tags; the original event and author remain the source.", field("Hydra topics", "text", "communities", item.topics?.join(", ") ?? "", "Separate /h/ topic names with commas.", { required: true, placeholder: "science, biology" }), {
    submitLabel: "Publish repost",
    onSubmit: (data) => mutate("nostr.curate", { persona_id: persona.id, event_json: item.event, communities: parseCommunities(data.get("communities")) }, "Reposted to the selected Hydra topics."),
  });
}

function renderRedditBridge() {
  const persona = activePersona(session.state);
  const settings = session.state.settings ?? {};
  const projections = (session.state.projections ?? []).filter((item) => item.personaId === persona.id);
  const importedWriting = (session.state.objects ?? [])
    .filter((item) => item.author === persona.publicKey && item.externalSource)
    .sort((left, right) => right.editedAt - left.editedAt);
  const visibleImportedWriting = importedWriting.slice(0, 25);
  const duplicateGroups = new Map();
  for (const projection of projections.filter((item) => !["abandoned", "withdrawn"].includes(item.state))) {
    const key = `${projection.anchor}\n${projection.destinationSystem}\n${projection.destination}`;
    duplicateGroups.set(key, (duplicateGroups.get(key) ?? 0) + 1);
  }
  const header = viewHeader("Reddit Bridge");
  const body = element("div", { class: "form-page" }, [
    element("section", { class: "context-card" }, [
      element("h2", { text: persona.redditLinked ? "Reddit connected" : "No Reddit account linked" }),
      actionButton(persona.redditLinked ? "Disconnect Reddit" : "Connect with Reddit OAuth", persona.redditLinked ? disconnectReddit : connectReddit, persona.redditLinked ? "danger-button" : "primary-button"),
      persona.redditLinked ? actionButton(persona.redditProof ? "Replace public identity proof" : "Publish optional identity proof", showRedditIdentityProof) : null,
      persona.redditProof ? element("p", { class: "evidence-note", text: `Public proof: ${persona.redditProof}` }) : null,
    ]),
    element("section", { class: "context-card" }, [
      element("h2", { text: "Continuity systems" }),
      element("p", { text: "Big Stick and Reddacted apply only to Reddit copies that began as Hydra content." }),
      actionButton("Install Firefox companion", installFirefox),
    ]),
    element("section", { class: "context-card" }, [
      element("h2", { text: "Import Reddit writing" }),
      element("p", { text: "Import only your posts and comments from Reddit’s official account-data export. Other export files are ignored." }),
      actionButton("Import official data export", showRedditExportImport, "primary-button"),
    ]),
    importedWriting.length ? element("section", { class: "context-card imported-writing" }, [
      element("h2", { text: `Imported Reddit writing (${importedWriting.length})` }),
      ...visibleImportedWriting.map((item) => element("article", { class: "imported-writing-item" }, [
        element("div", { class: "meta-line" }, [
          element("span", { class: "provenance reddit", text: item.kind === "comment" ? "Reddit comment" : "Reddit post" }),
          element("span", { text: item.communities?.length ? item.communities.map((community) => `/r/${community}`).join(" · ") : "Reddit" }),
          element("span", { text: relativeTime(item.editedAt) }),
        ]),
        item.title ? element("strong", { text: visibleInlineText(item.title) }) : null,
        element("p", { class: "post-body", text: visibleInlineText(item.body) }),
        element("p", { class: "source-link", text: item.externalSource }),
        actionButton("Copy source link", () => copyText(item.externalSource, "Reddit source link copied.")),
      ])),
    ]) : null,
    projections.length ? element("h2", { text: `Projection records (${projections.length})` }) : null,
    ...projections.map((projection) => {
      const duplicateKey = `${projection.anchor}\n${projection.destinationSystem}\n${projection.destination}`;
      const duplicateCount = duplicateGroups.get(duplicateKey) ?? 0;
      return element("section", { class: "context-card" }, [
      element("strong", { text: projection.externalUrl || projection.destination }),
      element("p", { text: `${projection.state}${projection.divergence ? ` · ${projection.divergence}` : ""}` }),
      duplicateCount > 1 ? element("p", { class: "evidence-note", text: `${duplicateCount} active mappings exist for this exact Hydra object and Reddit destination. Choose which local mapping Hydra should keep.` }) : null,
      projection.error ? element("p", { text: projection.error }) : null,
      element("div", { class: "post-actions" }, [
        duplicateCount > 1 ? actionButton("Keep this mapping", () => resolveProjectionDuplicates(projection), "primary-button") : null,
        actionButton("Sync", () => projectionAction("reddit.projection.sync", projection.id, "Projection synchronized.")),
        actionButton(projection.syncEnabled ? "Disable auto-sync" : "Enable auto-sync", () => mutate("reddit.projection.sync_setting", { projection_id: projection.id, enabled: !projection.syncEnabled }, projection.syncEnabled ? "Automatic Hydra-to-Reddit edits disabled for this copy." : "Automatic Hydra-to-Reddit edits enabled for this copy.")),
        projection.divergence ? actionButton("Adopt Reddit edit", () => projectionAction("reddit.divergence.adopt", projection.id, "Reddit revision adopted as a new Hydra head.")) : null,
        projection.divergence ? actionButton("Restore Hydra to Reddit", () => projectionAction("reddit.divergence.restore", projection.id, "Canonical Hydra content restored to Reddit.")) : null,
        projection.divergence ? actionButton("Keep both", () => projectionAction("reddit.divergence.keep", projection.id, "Both versions retained; Hydra head remains canonical.")) : null,
        settings.continuity?.big_stick_enabled !== false && projection.state !== "withdrawn" ? actionButton("Big Stick", () => showBigStick(projection)) : null,
        settings.continuity?.reddacted_enabled !== false && projection.state !== "withdrawn" ? actionButton("Reddact", () => showReddact(projection), "danger-button") : null,
      ]),
    ]);
    }),
  ]);
  view.replaceChildren(header, body);
}

function showRedditExportImport() {
  modal("Import Reddit writing", "Choose the Reddit account-data ZIP or its extracted folder. Only posts.csv and comments.csv are read.", element("div", { class: "community-actions" }, [
    actionButton("Choose ZIP", () => chooseRedditExport(false), "primary-button"),
    actionButton("Choose extracted folder", () => chooseRedditExport(true)),
  ]), { submitLabel: "Close", onSubmit: closeModal });
}

async function chooseRedditExport(directory) {
  if (!desktopDialog) { toast("The desktop file chooser is unavailable.", true); return; }
  const persona = activePersona(session.state);
  const path = await desktopDialog.open({ multiple: false, directory, filters: directory ? undefined : [{ name: "Reddit account data", extensions: ["zip"] }] });
  if (!path) return;
  const preview = await runtime("reddit.export.preview", { path });
  const result = preview.result ?? preview;
  const items = result.items ?? [];
  const checklist = items.map((item) => element("label", { class: "selection-item" }, [
    element("input", { type: "checkbox", name: "selected", value: item.fullname, checked: "checked" }),
    element("span", {}, [
      element("strong", { text: item.title || item.body?.slice(0, 80) || item.fullname }),
      element("small", { class: "field-help", text: `${item.kind} · ${item.subreddit ? `/r/${item.subreddit}` : "unknown community"}` }),
    ]),
  ]));
  modal("Import Reddit writing", `${result.posts ?? 0} posts and ${result.comments ?? 0} comments found. Messages, votes, IP logs, and other export files are ignored.`, element("div", {}, [
    ...checklist,
    toggle("Publish imported writing to Nostr", "publish", false, "Off keeps the imported posts and comments only in this local Hydra library."),
  ]), { submitLabel: "Import selected writing", onSubmit: (data) => {
    const selected = data.getAll("selected").map(String);
    if (!selected.length) throw new Error("Select at least one post or comment to import.");
    return mutate("reddit.export.import", { persona_id: persona.id, path, selected, publish: Boolean(data.get("publish")) }, "Selected Reddit writing imported.");
  } });
}

async function saveThemeChoice(event) {
  const select = event.currentTarget;
  const previous = session.state.settings?.theme ?? "system";
  const selected = select.value;
  document.documentElement.dataset.theme = selected;
  setBusy(true);
  try {
    const result = await runtime("settings.update", { theme: selected });
    const snapshot = extractState(result);
    if (snapshot?.personas) session.state = snapshot;
    else session.state.settings.theme = selected;
    toast("Theme saved locally.");
  } catch (error) {
    select.value = previous;
    document.documentElement.dataset.theme = previous;
    toast(readableError(error), true);
  } finally {
    setBusy(false);
  }
}

function settingsGroup(title, children, open = false) {
  return element("details", { class: "settings-group", open: open ? "open" : null }, [
    element("summary", { text: title }),
    element("div", { class: "settings-group-body" }, children),
  ]);
}

async function openStorageFolder(folder) {
  setBusy(true);
  try {
    await runtime("storage.open", { folder });
  } catch (error) {
    toast(readableError(error), true);
  } finally {
    setBusy(false);
  }
}

function renderSettings() {
  const settings = session.state.settings ?? {};
  const persona = activePersona(session.state);
  const header = viewHeader("Settings");
  const relayValue = (settings.relays ?? []).join("\n");
  const personaRelaySettings = settings.persona_relays?.[persona.id] ?? {};
  const personaReadRelayValue = (personaRelaySettings.read ?? settings.relays ?? []).join("\n");
  const personaWriteRelayValue = (personaRelaySettings.write ?? settings.relays ?? []).join("\n");
  const inboxRelayValue = (settings.inbox_relays ?? []).join("\n");
  const communityOverrides = Object.entries(settings.community_crosspost_defaults ?? {}).map(([community, enabled]) => `${community}=${enabled ? "on" : "off"}`).join("\n");
  const blobServers = (settings.persona_blob_servers?.[persona.id] ?? []).join("\n");
  const follows = (session.state.follows ?? []).filter((item) => item.personaId === persona.id);
  const publicFollowSets = (session.state.publicFollowSets ?? []).filter((item) => item.personaId === persona.id);
  const blocks = (session.state.blocks ?? []).filter((item) => item.personaId === persona.id);
  const blockExceptions = (session.state.blockExceptions ?? []).filter((item) => item.personaId === persona.id);
  const blockSources = (session.state.blockSources ?? []).filter((item) => item.personaId === persona.id);
  const silences = (session.state.silences ?? []).filter((item) => item.personaId === persona.id);
  const silenceExceptions = (session.state.silenceExceptions ?? []).filter((item) => item.personaId === persona.id);
  const silenceSources = (session.state.silenceSources ?? []).filter((item) => item.personaId === persona.id);
  const hides = (session.state.hides ?? []).filter((item) => item.personaId === persona.id);
  const hideExceptions = (session.state.hideExceptions ?? []).filter((item) => item.personaId === persona.id);
  const hideSources = (session.state.hideSources ?? []).filter((item) => item.personaId === persona.id);
  const removals = (session.state.removals ?? []).filter((item) => item.personaId === persona.id);
  const restorations = (session.state.restorations ?? []).filter((item) => item.personaId === persona.id);
  const removalSources = (session.state.removalSources ?? []).filter((item) => item.personaId === persona.id);
  const filters = (session.state.filters ?? []).filter((item) => item.personaId === persona.id);
  const drafts = (session.state.drafts ?? []).filter((item) => item.personaId === persona.id);
  const storage = session.state.storage ?? { root: session.state.durableRoot, media: `${session.state.durableRoot}/media`, mediaExists: false };
  const feedWeights = { followed: 100, communities: 100, replies: 100, revisit: 100, ...(settings.feed_source_weights ?? {}) };
  const body = element("form", { class: "form-page", onsubmit: saveSettings }, [
    field("Public display name", "text", "display_name", persona.displayName, "", { required: true }),
    field("Theme", "select", "theme", settings.theme ?? "system", "", { values: [["system", "Follow system"], ["light", "Light"], ["dark", "Dark"]], onchange: saveThemeChoice }),
    settingsGroup("Advanced settings", [
      element("h2", { class: "settings-subheading", text: "Nostr and media" }),
      toggle(
        "Show Book Club cross-links",
        "book_club_cross_links",
        settings.cross_links?.book_club_enabled !== false,
        session.companions.bookClubInstalled
          ? "Shows direct handoffs for verified Nostr events. Shared Nostr support remains available when off."
          : "No installed Book Club link handler was found, so direct cross-links are unavailable.",
        { disabled: !session.companions.bookClubInstalled },
      ),
      field("Default relays", "textarea", "relays", relayValue, "Fallback for personas without relay preferences."),
      field("This persona's read relays", "textarea", "persona_read_relays", personaReadRelayValue, "Published as NIP-65 read preferences."),
      field("This persona's write relays", "textarea", "persona_write_relays", personaWriteRelayValue, "Published as NIP-65 write preferences."),
      field("Private-message inbox relays", "textarea", "inbox_relays", inboxRelayValue, "One to three published NIP-17 inbox relays."),
      field("Replication threshold", "number", "replication", settings.replication_threshold ?? 2, "One relay means published; this many means replicated."),
      field("Optional Nostr web gateway", "text", "preferred_gateway", settings.continuity?.preferred_gateway_template ?? "", "HTTPS template, for example https://njump.me/{identifier}."),
      field("Local spam threshold", "number", "spam_threshold", settings.spam_filter_threshold ?? 100, "0 disables hiding; 100 requires every strong local signal.", { min: 0, max: 100 }),
      field("Remote and sensitive media", "select", "remote_media_policy", settings.remote_media_policy ?? "on_demand", "", { values: [["never", "Never fetch"], ["on_demand", "Ask before loading"]] }),
      toggle("Preserve media copies", "media_copy", settings.media_copy_enabled !== false, "Off retains URLs and text without copying files."),
      field("Maximum copied media (MiB)", "number", "max_media_mib", Math.round((settings.max_media_bytes ?? 26214400) / 1048576), "", { min: 1 }),
      field("Content-addressed blob servers", "textarea", "blob_servers", blobServers, "Optional; local preservation never depends on them."),
      element("h2", { class: "settings-subheading", text: "My Feed sources" }),
      element("p", { text: "Relative local weights; equal values have equal priority." }),
      field("Followed personas", "number", "feed_followed", feedWeights.followed, "", { min: 0, max: 200 }),
      field("Subscribed communities", "number", "feed_communities", feedWeights.communities, "", { min: 0, max: 200 }),
      field("Replies involving me", "number", "feed_replies", feedWeights.replies, "", { min: 0, max: 200 }),
      field("Revisit memory", "number", "feed_revisit", feedWeights.revisit, "", { min: 0, max: 200 }),
      element("h2", { class: "settings-subheading", text: "Reddit projection" }),
      toggle("Crosspost to Reddit by default", "crosspost", Boolean(settings.crosspost_default), "The composer always allows an override."),
      field("This persona’s default", "select", "persona_crosspost", crosspostOverride(settings.persona_crosspost_defaults?.[persona.id]), "", { values: [["inherit", "Inherit"], ["on", "Always on"], ["off", "Always off"]] }),
      field("Posts", "select", "post_crosspost", crosspostOverride(settings.content_crosspost_defaults?.post), "", { values: [["inherit", "Inherit"], ["on", "Always on"], ["off", "Always off"]] }),
      field("Comments", "select", "comment_crosspost", crosspostOverride(settings.content_crosspost_defaults?.comment), "", { values: [["inherit", "Inherit"], ["on", "Always on"], ["off", "Always off"]] }),
      field("Community overrides", "textarea", "community_crossposts", communityOverrides, "One per line: science=on or science=off."),
      element("h2", { class: "settings-subheading", text: "Continuity" }),
      field("Replication threshold", "number", "continuity_replication", settings.continuity?.replication_threshold ?? 0, "0 inherits the ordinary threshold.", { min: 0 }),
      toggle("Enable Big Stick", "big_stick_enabled", settings.continuity?.big_stick_enabled !== false, "Opt-in for each projection."),
      field("Big Stick preservation level", "select", "big_stick_archive_level", settings.continuity?.big_stick_archive_level ?? "item", "", { values: [["item", "Item only"], ["ancestors", "Hydra item + Hydra ancestors"], ["visible_siblings", "Hydra context currently loaded"], ["loaded_thread", "Hydra thread currently loaded"]] }),
      toggle("Enable Reddacted", "reddacted_enabled", settings.continuity?.reddacted_enabled !== false, "One-way withdrawal of Hydra-originated Reddit projections."),
      field("Reddacted preservation level", "select", "reddacted_archive_level", settings.continuity?.reddacted_archive_level ?? "item", "", { values: [["item", "Item only"], ["ancestors", "Hydra item + Hydra ancestors"], ["visible_siblings", "Hydra context currently loaded"], ["loaded_thread", "Hydra thread currently loaded"]] }),
    ]),
    element("div", { class: "modal-actions" }, [actionButton("Save settings", null, "primary-button")]),
    element("section", { class: "context-card" }, [
      element("h2", { text: "Local data storage" }),
      element("p", { text: "All local data is under this folder. Synced posts, comments, subscriptions, and history are stored in an encrypted event log—not as loose Markdown files." }),
      element("p", { class: "source-link", text: storage.root }),
      element("div", { class: "post-actions" }, [
        actionButton("Open Hydra data folder", () => openStorageFolder("data"), "primary-button"),
        storage.mediaExists ? actionButton("Open preserved media folder", () => openStorageFolder("media")) : null,
      ]),
      storage.mediaExists
        ? element("p", { text: "Preserved attachments are separate content-addressed files in the media folder." })
        : element("p", { text: "No preserved media folder exists yet. It is created when the first attachment is preserved. Posts are not stored in separate persona folders." }),
    ]),
    drafts.length ? element("section", { class: "context-card" }, [
      element("h2", { text: `Private drafts (${drafts.length})` }),
      element("p", { text: "Drafts are encrypted, persona-bound, and never sent to a relay or public media server." }),
      ...drafts.map((draft) => element("div", { class: "readiness-row" }, [
        element("div", {}, [element("strong", { text: draft.title || "Untitled draft" }), element("p", { text: `Updated ${relativeTime(draft.updatedAt)} · ${draft.kind}` })]),
        element("div", { class: "post-actions" }, [
          draft.kind === "post" ? actionButton("Continue", () => showPostComposer(draft)) : null,
          actionButton("Discard", () => mutate("draft.discard", { persona_id: persona.id, id: draft.id }, "Draft discarded for this persona."), "danger-button"),
        ]),
      ])),
    ]) : null,
    element("section", { class: "context-card" }, [
      element("h2", { text: "Privacy" }),
      element("p", { text: "Personas are pseudonymous, not guaranteed anonymous. Timing, relays, media servers, IP addresses, writing style, and mistakes can correlate separate keys. Telemetry is off by default." }),
    ]),
    element("section", { class: "context-card" }, [
      element("h2", { text: "People" }),
      element("p", { text: `${session.state.followCount ?? 0} follows · ${session.state.blockCount ?? 0} blocks · ${session.state.silenceCount ?? 0} silences. Public declarations are signed claims, not moderation decisions.` }),
      element("div", { class: "post-actions" }, [
        actionButton("Follow a persona", showFollowEditor),
        actionButton("Publish a follow set", showFollowSetEditor),
        actionButton("Block locally", showBlockEditor, "danger-button"),
        actionButton("Silence a persona", showSilenceEditor),
        actionButton("Follow blocks from…", showBlockSourceEditor),
        actionButton("Follow silences from…", showSilenceSourceEditor),
      ]),
      ...follows.map((item) => element("div", { class: "readiness-row" }, [
        element("div", {}, [element("strong", { text: `${item.target.slice(0, 18)}…` }), element("p", { text: item.public ? "Public follow" : "Private follow" })]),
        actionButton("Unfollow", () => mutate("follow.set", { persona_id: persona.id, target: item.target, public: item.public, following: false }, "Follow removed from this persona.")),
      ])),
      ...publicFollowSets.map((item) => element("div", { class: "readiness-row" }, [
        element("div", {}, [
          element("strong", { text: item.title }),
          element("p", { text: `Public NIP-51 follow set · ${item.members.length} selected persona${item.members.length === 1 ? "" : "s"}` }),
        ]),
        actionButton("Revise", () => showFollowSetEditor(item)),
      ])),
      ...blocks.map((item) => element("div", { class: "readiness-row" }, [
        element("div", {}, [element("strong", { text: `${item.target.slice(0, 18)}…` }), element("p", { text: `${item.public ? "Published block" : "Private block"} · ${item.scope === "global" ? "everywhere" : item.scope.replace(/^topic:/, "/h/")}${item.reason ? ` · ${item.reason}` : ""}` })]),
        actionButton("Unblock", () => mutate("block.set", { persona_id: persona.id, target: item.target, public: item.public, blocked: false, action: "unblock", topic: item.scope.startsWith("topic:") ? item.scope.slice(6) : null, reason: null }, "Block judgment reversed.")),
      ])),
      ...blockExceptions.map((item) => element("div", { class: "readiness-row" }, [
        element("div", {}, [
          element("strong", { text: `${item.target.slice(0, 18)}…` }),
          element("p", { text: `${item.public ? "Published unblock" : "Private unblock"} · ${item.scope === "global" ? "everywhere" : item.scope.replace(/^topic:/, "/h/")}` }),
        ]),
        actionButton("Return to followed blocks", () => mutate("block.set", { persona_id: persona.id, target: item.target, public: item.public, blocked: false, action: "withdraw", topic: item.scope.startsWith("topic:") ? item.scope.slice(6) : null, reason: null }, "Direct unblock withdrawn; followed judgments apply again.")),
      ])),
      ...blockSources.map((item) => element("div", { class: "readiness-row" }, [
        element("div", {}, [
          element("strong", { text: `${item.source.slice(0, 18)}…` }),
          element("p", { text: `Following blocks ${[item.global ? "everywhere" : null, ...(item.topics ?? []).map((topic) => `/h/${topic}`)].filter(Boolean).join(", ")} · priority ${item.rank} · ${item.completeness}` }),
        ]),
        actionButton("Stop following blocks", () => mutate("block_source.set", { persona_id: persona.id, source: item.source, global: false, topics: [], rank: item.rank, enabled: false }, "Block source removed.")),
      ])),
      ...silences.map((item) => element("div", { class: "readiness-row" }, [
        element("div", {}, [
          element("strong", { text: `${item.target.slice(0, 18)}…` }),
          element("p", { text: `${item.public ? "Published silence" : "Private silence"} · ${item.scope === "global" ? "everywhere" : item.scope.replace(/^topic:/, "/h/")}${item.cutoff ? ` · new activity since ${new Date(item.cutoff * 1000).toLocaleString()}` : ""}${item.reason ? ` · ${item.reason}` : ""}` }),
        ]),
        actionButton("Unsilence", () => mutate("silence.set", { persona_id: persona.id, target: item.target, public: item.public, silenced: false, action: "unsilence", topic: item.scope.startsWith("topic:") ? item.scope.slice(6) : null, reason: null }, "Silence judgment reversed.")),
      ])),
      ...silenceExceptions.map((item) => element("div", { class: "readiness-row" }, [
        element("div", {}, [
          element("strong", { text: `${item.target.slice(0, 18)}…` }),
          element("p", { text: `${item.public ? "Published unsilence" : "Private unsilence"} · ${item.scope === "global" ? "everywhere" : item.scope.replace(/^topic:/, "/h/")}` }),
        ]),
        actionButton("Return to followed silences", () => mutate("silence.set", { persona_id: persona.id, target: item.target, public: item.public, silenced: false, action: "withdraw", topic: item.scope.startsWith("topic:") ? item.scope.slice(6) : null, reason: null }, "Direct unsilence withdrawn; followed judgments apply again.")),
      ])),
      ...silenceSources.map((item) => element("div", { class: "readiness-row" }, [
        element("div", {}, [
          element("strong", { text: `${item.source.slice(0, 18)}…` }),
          element("p", { text: `Following silences ${[item.global ? "everywhere" : null, ...(item.topics ?? []).map((topic) => `/h/${topic}`)].filter(Boolean).join(", ")} · priority ${item.rank} · ${item.completeness}` }),
        ]),
        actionButton("Stop following silences", () => mutate("silence_source.set", { persona_id: persona.id, source: item.source, global: false, topics: [], rank: item.rank, enabled: false }, "Silence source removed.")),
      ])),
    ]),
    element("section", { class: "context-card" }, [
      element("h2", { text: "Content judgments" }),
      element("p", { text: `${session.state.hideCount ?? 0} hides · ${session.state.removalCount ?? 0} community removals. These judgments change this persona’s view without deleting events.` }),
      element("div", { class: "post-actions" }, [
        actionButton("Follow hides from…", () => showContentSourceEditor("hide")),
        actionButton("Follow removals from…", () => showContentSourceEditor("removal")),
      ]),
      ...hides.map((item) => element("div", { class: "readiness-row" }, [
        element("div", {}, [element("strong", { text: `${item.target.slice(0, 18)}…` }), element("p", { text: `${item.public ? "Published hide" : "Private hide"} · ${item.scope === "global" ? "everywhere" : item.scope.replace(/^topic:/, "/h/")}${item.reason ? ` · ${item.reason}` : ""}` })]),
        actionButton("Unhide", () => mutate("hide.set", { persona_id: persona.id, target: item.target, public: item.public, action: "unhide", topic: item.scope.startsWith("topic:") ? item.scope.slice(6) : null, reason: null }, "Hide judgment reversed.")),
      ])),
      ...hideExceptions.map((item) => element("div", { class: "readiness-row" }, [
        element("div", {}, [element("strong", { text: `${item.target.slice(0, 18)}…` }), element("p", { text: `${item.public ? "Published unhide" : "Private unhide"} · ${item.scope === "global" ? "everywhere" : item.scope.replace(/^topic:/, "/h/")}` })]),
        actionButton("Return to followed hides", () => mutate("hide.set", { persona_id: persona.id, target: item.target, public: item.public, action: "withdraw", topic: item.scope.startsWith("topic:") ? item.scope.slice(6) : null, reason: null }, "Direct unhide withdrawn; followed judgments apply again.")),
      ])),
      ...removals.map((item) => element("div", { class: "readiness-row" }, [
        element("div", {}, [element("strong", { text: `${item.target.slice(0, 18)}…` }), element("p", { text: `${item.public ? "Published removal" : "Private removal"} · ${item.scope.replace(/^topic:/, "/h/")}${item.reason ? ` · ${item.reason}` : ""}` })]),
        actionButton("Restore", () => mutate("membership.set", { persona_id: persona.id, target: item.target, public: item.public, action: "restore", topic: item.scope.slice(6), reason: null }, "Community membership restored.")),
      ])),
      ...restorations.map((item) => element("div", { class: "readiness-row" }, [
        element("div", {}, [element("strong", { text: `${item.target.slice(0, 18)}…` }), element("p", { text: `${item.public ? "Published restoration" : "Private restoration"} · ${item.scope.replace(/^topic:/, "/h/")}` })]),
        actionButton("Return to followed removals", () => mutate("membership.set", { persona_id: persona.id, target: item.target, public: item.public, action: "withdraw", topic: item.scope.slice(6), reason: null }, "Direct restoration withdrawn; followed judgments apply again.")),
      ])),
      ...hideSources.map((item) => contentSourceRow(item, "hide")),
      ...removalSources.map((item) => contentSourceRow(item, "removal")),
    ]),
    element("section", { class: "context-card" }, [
      element("h2", { text: "Vote review" }),
      element("p", { text: "Revisit recent and old stances without erasing their history. Reaffirmation remains subject to the 18-hour credit interval." }),
      actionButton("Review my votes", showVoteReview),
    ]),
    element("section", { class: "context-card" }, [
      element("h2", { text: "Local defenses" }),
      element("p", { text: "Encrypted filters alter only this persona’s lens and do not remove events from Nostr or moderate a community." }),
      actionButton("Add local filter", showLocalFilterEditor),
      ...filters.map((item) => element("div", { class: "readiness-row" }, [
        element("div", {}, [element("strong", { text: item.value }), element("p", { text: `${item.kind} filter` })]),
        actionButton("Remove", () => mutate("filter.set", { persona_id: persona.id, kind: item.kind, value: item.value, enabled: false }, "Local filter removed.")),
      ])),
    ]),
    element("section", { class: "context-card" }, [
      element("h2", { text: "Encrypted persona archive" }),
      element("p", { text: "Exports include this persona’s key, signed events, durable memory, projection mappings, media manifests, and relay receipts—but no other local persona’s secret." }),
      actionButton("Back up this persona", showBackupExport, "primary-button"),
    ]),
    element("section", { class: "context-card" }, [
      element("h2", { text: "Raw local evidence" }),
      element("p", { text: "Inspect the verified append-only ledger even when a local lens hides an item. Encrypted private payloads remain ciphertext." }),
      actionButton("Inspect raw events", showRawEvidence),
    ]),
  ]);
  view.replaceChildren(header, body);
}

function renderWelcome() {
  const header = viewHeader("Welcome to Hydra");
  const body = element("div", { class: "content-list" }, [
    emptyState("Create a persona", "A persona is a durable public Nostr identity.", "Create persona", showPersonaCreator),
    element("section", { class: "context-card" }, [
      element("h2", { text: "Restore a persona" }),
      actionButton("Restore encrypted archive", showBackupRestore),
    ]),
  ]);
  view.replaceChildren(header, body);
}

function renderUnavailable(error) {
  finishBoot();
  document.querySelector("#app").setAttribute("aria-busy", "false");
  view.replaceChildren(viewHeader("Hydra could not open"), element("div", { class: "content-list" }, [emptyState("Local runtime unavailable", readableError(error), "Try again", refresh)]));
  contextPanel.replaceChildren();
}

function field(label, type, name, value = "", help = "", options = {}) {
  let control;
  if (type === "textarea") control = element("textarea", { name, text: value, required: options.required ?? false, onchange: options.onchange });
  else if (type === "select") {
    control = element("select", { name, onchange: options.onchange }, options.values.map(([id, text]) => element("option", { value: id, text, selected: id === value ? "selected" : null })));
  } else control = element("input", { name, type, value, required: options.required ?? false, placeholder: options.placeholder ?? null, min: options.min ?? null, onchange: options.onchange });
  return element("label", { class: "field" }, [element("span", { text: label }), control, help ? element("small", { class: "field-help", text: help }) : null]);
}

function toggle(label, name, checked, help, options = {}) {
  return element("label", { class: "toggle-row" }, [
    element("span", {}, [element("strong", { text: label }), element("small", { class: "field-help", text: help })]),
    element("input", { type: "checkbox", name, checked: checked ? "checked" : null, disabled: options.disabled ?? false }),
  ]);
}

function modal(title, subtitle, body, { submitLabel = "Continue", onSubmit, danger = false } = {}) {
  const form = element("form", { class: "modal", role: "dialog", "aria-modal": "true", "aria-labelledby": "modal-title", onsubmit: async (event) => {
    event.preventDefault();
    const submit = form.querySelector("button[type=submit]");
    submit.disabled = true;
    try {
      await onSubmit?.(new FormData(form));
    } catch (error) {
      if (!error?.hydraSurfaced) toast(readableError(error), true);
    } finally {
      submit.disabled = false;
    }
  } }, [
    element("header", { class: "modal-header" }, [
      element("div", {}, [element("h2", { id: "modal-title", text: title }), element("p", { text: subtitle })]),
      element("button", { type: "button", class: "icon-button", text: "×", "aria-label": "Close", onclick: closeModal }),
    ]),
    element("div", { class: "modal-body" }, [body, element("div", { class: "modal-actions" }, [
      actionButton("Cancel", closeModal),
      element("button", { type: "submit", class: danger ? "danger-button" : "primary-button", text: submitLabel }),
    ])]),
  ]);
  const backdrop = element("div", { class: "modal-backdrop", onclick: (event) => { if (event.target === backdrop) closeModal(); } }, [form]);
  modalRoot.replaceChildren(backdrop);
  window.setTimeout(() => form.querySelector("input, textarea, select, button")?.focus(), 0);
}

function closeModal() { modalRoot.replaceChildren(); }

function showPersonaCreator() {
  modal("Create a Hydra persona", "Creates one Nostr identity without an email address or a public link to other personas.", element("div", {}, [
    field("Display name", "text", "display_name", "", "This name is stable across Hydra for this persona.", { required: true, placeholder: "Display name" }),
  ]), { submitLabel: "Create persona", onSubmit: (data) => mutate("persona.create", { display_name: data.get("display_name") }, "Persona created locally and queued for publication.") });
}

function showPersonaMenu() {
  const personas = session.state?.personas ?? [];
  const body = element("div", {}, [
    ...personas.map((persona) => element("button", { type: "button", class: "nav-item", onclick: async () => {
      await mutate("persona.switch", { persona_id: persona.id }, `Switched to ${persona.displayName}.`);
    } }, [element("span", { text: persona.displayName.slice(0, 1).toUpperCase() }), element("span", { text: persona.displayName }), persona.active ? element("span", { text: "Active" }) : null])),
    actionButton("Create another persona", () => { closeModal(); showPersonaCreator(); }),
    actionButton("Import an existing Nostr key", () => { closeModal(); showPersonaImporter(); }),
    actionButton("Connect an external signer", () => { closeModal(); showRemotePersonaConnector(); }),
  ]);
  modal("Switch persona", "Drafts, Reddit credentials, notifications, and private state remain persona-bound.", body, { submitLabel: "Close", onSubmit: closeModal });
}

function showPersonaImporter() {
  modal("Import a Nostr persona", "The private key goes directly to Hydra’s native credential vault and is never stored in browser storage.", element("div", {}, [
    field("Display name", "text", "display_name", "", "", { required: true }),
    field("Private key", "password", "secret", "", "Accepts an nsec or supported secret-key encoding.", { required: true }),
  ]), { submitLabel: "Import persona", onSubmit: (data) => mutate("persona.import", { display_name: data.get("display_name"), secret: data.get("secret") }, "Persona imported into secure local custody.") });
}

function showRemotePersonaConnector() {
  modal("Connect an external signer", "Hydra uses the standard Nostr remote-signer flow; the signing key remains outside Hydra.", element("div", {}, [
    field("Display name", "text", "display_name", "", "", { required: true }),
    field("Nostr Connect bunker URI", "text", "bunker_uri", "", "Paste the bunker:// URI supplied by your signer.", { required: true }),
  ]), { submitLabel: "Connect signer", onSubmit: (data) => mutate("persona.connect_remote", { display_name: data.get("display_name"), bunker_uri: data.get("bunker_uri") }, "External signer connected as a separate persona.") });
}

function showComposer(defaultCommunity = null) {
  showPostComposer(null, defaultCommunity);
}

function showPostComposer(draft = null, defaultCommunity = null) {
  const persona = activePersona(session.state);
  if (!persona) { showPersonaCreator(); return; }
  const draftId = draft?.id ?? crypto.randomUUID();
  const body = element("div", {}, [
    field("Title", "text", "title", draft?.title ?? "", "", { required: true }),
    field("Communities", "text", "communities", draft?.communities?.join(", ") || defaultCommunity || "", "Separate several ownerless /h/ coordinates with commas.", { required: true, placeholder: "science, biology" }),
    field("Post", "textarea", "body", draft?.body ?? "", "The post is stored in Hydra. A Reddit projection is optional.", { required: true }),
    toggle("Crosspost to Reddit", "crosspost", configuredCrosspostDefault("post", defaultCommunity), "Off by default. Attribution is also off unless selected later."),
    actionButton("Save encrypted draft", async () => {
      const data = new FormData(modalRoot.querySelector("form"));
      await mutate("draft.save", { id: draftId, persona_id: persona.id, kind: "post", title: data.get("title"), body: data.get("body"), communities: parseCommunities(data.get("communities")), parent: null }, "Draft saved only for this persona.");
    }),
  ]);
  modal(draft ? "Continue Hydra draft" : "New Hydra post", `Posting as ${persona.displayName}`, body, { submitLabel: "Publish to Hydra", onSubmit: async (data) => {
    const communities = parseCommunities(data.get("communities"));
    const response = await mutate("post.create", { persona_id: persona.id, title: data.get("title"), body: data.get("body"), communities }, "Post saved locally and queued for its selected relays.");
    if (draft) {
      await runtime("draft.discard", { persona_id: persona.id, id: draft.id });
      session.state = extractState(await runtime("state"));
      render();
    }
    if (data.get("crosspost")) showPostProjection(response.result.anchor, communities);
  } });
}

function showPostProjection(anchor, communities) {
  const persona = activePersona(session.state);
  const choices = communities.map((community) => element("label", { class: "selection-item" }, [
    element("input", { type: "checkbox", name: "community", value: community, checked: true }),
    element("span", {}, [element("strong", { text: `/r/${community}` }), element("small", { class: "field-help", text: "A separate Reddit projection feeding the same Hydra discussion." })]),
  ]));
  modal("Choose Reddit projections", `Each checked subreddit receives one projection from u/${persona.redditUsername || "linked account"}. This publicly links that Reddit account to this Hydra persona.`, element("div", {}, [
    ...choices,
    field("Attribution", "select", "attribution", "none", "Attribution is deliberately off by default.", { values: [["none", "No Hydra marker"], ["posted_from_hydra", "Posted from Hydra"]] }),
  ]), { submitLabel: "Project selected copies", onSubmit: async (data) => {
    const selected = data.getAll("community").map(String);
    if (!selected.length) { closeModal(); toast("Hydra post kept without a Reddit projection."); return; }
    const failures = [];
    for (const subreddit of selected) {
      try {
        const queued = await runtime("reddit.post.queue", { persona_id: persona.id, anchor, subreddit, attribution: data.get("attribution"), link: null });
        await runtime("reddit.projection.execute", { projection_id: queued.result.projectionId });
      } catch (error) {
        failures.push(`/r/${subreddit}: ${readableError(error)}`);
      }
    }
    closeModal();
    session.state = extractState(await runtime("state"));
    toast(failures.length ? `Post saved in Hydra. Reddit projection failures: ${failures.join(" · ")}` : "Selected Reddit projections published.", failures.length > 0);
    render();
  } });
}

async function setCommunitySubscription(community, subscribed, publicValue) {
  const persona = activePersona(session.state);
  try {
    await mutate("community.subscribe", { persona_id: persona.id, community, public: publicValue, subscribed }, subscribed ? (publicValue ? "Public subscription published." : "Community added privately to this persona’s feed.") : "Community removed from this persona’s feed.");
  } catch { /* mutation already surfaced the error */ }
}

function showNormComposer(community) {
  const persona = activePersona(session.state);
  modal("Propose a communal norm", `A signed proposition in /h/${community}, not a rule or removal power.`, field("Norm statement", "textarea", "statement", "", "Other personas may endorse, diverge, or reply with refinements.", { required: true }), {
    submitLabel: "Publish statement",
    onSubmit: (data) => mutate("norm.create", { persona_id: persona.id, statement: data.get("statement"), community }, "Norm statement published as one persona’s position."),
  });
}

function showReply(parent) {
  const persona = activePersona(session.state);
  const targets = redditReplyTargets(parent);
  const directTargets = targets.filter((target) => target.direct);
  const defaultCrosspost = persona.redditLinked
    && directTargets.length > 0
    && configuredCrosspostDefault("comment", parent.communities?.[0] ?? null);
  const targetChoices = targets.map((target) => element("label", { class: "selection-item" }, [
    element("input", {
      type: "checkbox",
      name: "reddit_parent",
      value: target.fullname,
      checked: defaultCrosspost && target.direct ? "checked" : null,
    }),
    element("span", {}, [
      element("strong", { text: target.label }),
      element("small", { class: "field-help", text: target.direct ? "Exact Reddit counterpart of this parent." : "Nearest projected ancestor; selecting it deliberately changes the Reddit reply point." }),
    ]),
  ]));
  modal("Reply in Hydra", `Replying as ${persona.displayName}. The reply is saved in Hydra before any optional Reddit projection.`, element("div", {}, [
    field("Reply", "textarea", "body", "", "Thread locks and subreddit bans do not prevent this Hydra reply.", { required: true }),
    targets.length && persona.redditLinked
      ? element("section", { class: "context-card" }, [
          element("strong", { text: `Optional Reddit projection as u/${persona.redditUsername || "linked account"}` }),
          element("p", { class: "evidence-note", text: "Crossposting publicly reveals the relationship between this Hydra persona and the selected Reddit account." }),
          ...targetChoices,
          field("Attribution", "select", "attribution", "none", "Hydra attribution is deliberately off by default.", { values: [["none", "No Hydra marker"], ["posted_from_hydra", "Posted from Hydra"]] }),
        ])
      : element("p", { class: "evidence-note", text: targets.length ? "Link this persona to Reddit before selecting a projection." : "No exact Reddit counterpart is available for this branch, so this reply remains Hydra-only." }),
  ]), { submitLabel: "Post reply", onSubmit: async (data) => {
    const created = await runtime("comment.create", { persona_id: persona.id, parent_anchor: parent.anchor, body: data.get("body") });
    const failures = [];
    for (const fullname of data.getAll("reddit_parent").map(String)) {
      try {
        const queued = await runtime("reddit.comment.queue", {
          persona_id: persona.id,
          anchor: created.result.anchor,
          parent: fullname,
          attribution: data.get("attribution") || "none",
          link: null,
        });
        await runtime("reddit.projection.execute", { projection_id: queued.result.projectionId });
      } catch (error) {
        failures.push(`${fullname}: ${readableError(error)}`);
      }
    }
    closeModal();
    session.state = extractState(await runtime("state"));
    toast(failures.length ? `Reply saved in Hydra. Reddit projection failures: ${failures.join(" · ")}` : data.getAll("reddit_parent").length ? "Reply saved in Hydra and projected to every selected Reddit parent." : "Reply saved in Hydra only.", failures.length > 0);
    render();
  } });
}

function redditReplyTargets(parent) {
  const seen = new Set();
  const targets = [];
  let current = parent;
  let direct = true;
  for (let depth = 0; current && depth < 64; depth += 1) {
    for (const projection of session.state.projections ?? []) {
      if (projection.anchor !== current.anchor || !isRedditDiscussionProjection(projection) || !projection.externalId || seen.has(projection.externalId)) continue;
      seen.add(projection.externalId);
      const subreddit = projection.externalUrl?.match(/reddit\.com\/r\/([a-z0-9_]+)/i)?.[1];
      targets.push({
        fullname: projection.externalId,
        direct,
        label: `${projection.externalId}${subreddit ? ` in /r/${subreddit}` : ""}`,
      });
    }
    if (targets.some((target) => target.direct) || current.kind === "post") break;
    current = (session.state.objects ?? []).find((item) => item.anchor === current.parent);
    direct = false;
  }
  return targets;
}

function showEdit(object) {
  const persona = activePersona(session.state);
  modal("Edit current version", "The immutable anchor and reply lineage remain unchanged. Hydra preserves observed head revisions locally.", element("div", {}, [
    object.kind !== "comment" ? field("Title", "text", "title", object.title || "", "", { required: true }) : null,
    object.kind === "post" ? field("Communities", "text", "communities", (object.communities ?? []).join(", "), "Add or remove ownerless /h/ coordinates without splitting the discussion.", { required: true }) : null,
    field("Body", "textarea", "body", object.body, "", { required: true }),
  ]), { submitLabel: "Save new head", onSubmit: (data) => mutate("object.edit", {
    persona_id: persona.id,
    anchor: object.anchor,
    title: data.get("title") || null,
    body: data.get("body"),
    communities: object.kind === "post" ? parseCommunities(data.get("communities")) : null,
  }, "New editable head published; replies remain attached.") });
}

function showDisown(object) {
  const persona = activePersona(session.state);
  modal("Request relay deletion", "This publishes a standard NIP-09 disowning request for the immutable anchor and current editable head. Relays and other users may retain prior events; deletion is not guaranteed.", field("Optional reason", "textarea", "reason", "", "Publicly signed with this persona; maximum 500 characters."), {
    submitLabel: "Publish disowning request",
    danger: true,
    onSubmit: (data) => mutate("object.disown", { persona_id: persona.id, anchor: object.anchor, reason: data.get("reason") || null }, "NIP-09 request queued. Local signed history retained; universal deletion is not guaranteed."),
  });
}

function showRevisit(object) {
  const persona = activePersona(session.state);
  const existing = (session.state.revisits ?? []).find((item) => item.personaId === persona.id && item.target === object.anchor);
  const intentions = [["return_soon", "Return soon"], ["reconsider_vote", "Reconsider my vote"], ["study", "Study"], ["notify_on_activity", "When discussion resumes"], ["review_on_date", "On a chosen date"], ["collection", "Place in a private collection"]];
  modal("Revisit this", "Private remembering is separate from public approval.", element("div", {}, [
    field("Intent", "select", "intent", "return_soon", "", { values: intentions }),
    field("Date", "date", "due", "", "Optional. Stored locally or in an encrypted Nostr list."),
    field("Private collection", "text", "collection", "", "Optional label visible only to this persona."),
    existing ? actionButton("Remove from Revisit", async () => {
      await mutate("revisit.remove", { persona_id: persona.id, target: object.anchor }, "Removed from this persona’s Revisit memory.");
    }, "danger-button") : null,
  ]), { submitLabel: "Add to Revisit", onSubmit: (data) => {
    const intent = data.get("intent");
    if (intent === "review_on_date" && !data.get("due")) throw new Error("Choose a date for a scheduled Revisit.");
    if (intent === "collection" && !String(data.get("collection") ?? "").trim()) throw new Error("Name the private collection.");
    const due = data.get("due") ? Math.floor(new Date(`${data.get("due")}T12:00:00`).getTime() / 1000) : null;
    return mutate("revisit.set", { persona_id: persona.id, target: object.anchor, intent, due_at: due, collection: data.get("collection") || null }, "Added to this persona’s private Revisit memory.");
  } });
}

function emojiReactionStrip(object) {
  const reactions = Object.entries(object.emojiReactions ?? {});
  return reactions.length ? element("div", { class: "status-row", "aria-label": "Emoji reactions" }, reactions.map(([emoji, count]) =>
    element("button", { type: "button", class: "community-chip", text: `${emoji} ${count}`, title: `React ${emoji}`, onclick: () => react(object.anchor, emoji) })
  )) : null;
}

function showEmojiReaction(object) {
  const presets = ["❤️", "🤔", "🔥", "😂", "👏", "💡"];
  modal("React with an emoji", "Emoji reactions are signed metadata and do not change Hydra’s vote score.", element("div", {}, [
    element("div", { class: "post-actions" }, presets.map((emoji) => actionButton(emoji, async () => {
      await react(object.anchor, emoji);
      closeModal();
    }))),
    field("Other emoji", "text", "emoji", "", "One short reaction, up to 32 characters."),
  ]), {
    submitLabel: "React",
    onSubmit: async (data) => {
      const emoji = String(data.get("emoji") ?? "").trim();
      if (!emoji || emoji.length > 32) throw new Error("Enter a short emoji reaction.");
      await react(object.anchor, emoji);
      closeModal();
    },
  });
}

function showVoteViews(object) {
  const rows = [
    ["Current score", object.currentScore ?? 0, "One current stance per persona."],
    ["Raw positive events", object.positiveReactions ?? 0, "Includes retained vote history and reaffirmations."],
    ["Raw negative events", object.negativeReactions ?? 0, "Includes retained vote history and reaffirmations."],
    ["Unique participants", object.uniqueVoters ?? 0, "Personas that have reacted at least once."],
    ["Persistence score", object.persistenceScore ?? object.currentScore ?? 0, "Current stances plus credited reaffirmations over time."],
    ["Trusted score", object.trustedScore ?? 0, "Current stances from this persona and personas it follows."],
    ["Reddit-linked score", object.redditLinkedScore ?? 0, "Current stances from locally verified Reddit-linked personas."],
  ];
  modal("Hydra vote views", "These scores are interpretations of signed reaction events; no score is authoritative.", element("div", { class: "content-list" }, rows.map(([label, value, detail]) => element("section", { class: "context-card" }, [
    element("div", { class: "meta-line" }, [element("strong", { text: label }), element("span", { class: "vote-score", text: String(value) })]),
    element("p", { text: detail }),
  ]))), { submitLabel: "Close", onSubmit: closeModal });
}

function showVoteReview() {
  const persona = activePersona(session.state);
  const byTarget = new Map();
  for (const reaction of session.state.reactions ?? []) {
    if (reaction.actor !== persona.publicKey || !["+", "-", "0"].includes(reaction.value)) continue;
    const current = byTarget.get(reaction.target);
    if (!current || reaction.occurredAt > current.occurredAt) byTarget.set(reaction.target, reaction);
  }
  const now = Math.floor(Date.now() / 1000);
  const entries = [...byTarget.values()].sort((a, b) => a.occurredAt - b.occurredAt);
  const renderEntry = (reaction) => {
    const object = (session.state.objects ?? []).find((item) => item.anchor === reaction.target);
    const label = object?.title || object?.body?.slice(0, 80) || reaction.target;
    const act = async (value) => { await react(reaction.target, value); showVoteReview(); };
    return element("article", { class: "context-card" }, [
      element("strong", { text: label }),
      element("p", { class: "evidence-note", text: `${reaction.value === "+" ? "Upvoted" : reaction.value === "-" ? "Downvoted" : "Neutral"} ${relativeTime(reaction.occurredAt)} ago · ${reaction.creditedReaffirmation ? "credited reaffirmation" : "current stance event"}` }),
      element("div", { class: "post-actions" }, [
        actionButton("Reaffirm +", () => act("+")),
        actionButton("Reverse −", () => act("-")),
        actionButton("Reset", () => act("0")),
      ]),
    ]);
  };
  const recent = entries.filter((item) => now - item.occurredAt < 30 * 86400);
  const old = entries.filter((item) => now - item.occurredAt >= 30 * 86400);
  const body = element("div", { class: "content-list" }, entries.length ? [
    element("h3", { text: `Old votes (${old.length})` }),
    ...old.map(renderEntry),
    element("h3", { text: `Recent votes (${recent.length})` }),
    ...recent.map(renderEntry),
  ] : [element("p", { text: "This persona has no votes to review yet." })]);
  modal("Vote-review queue", "Closing this view leaves votes unchanged. Repeat votes remain in the event history.", body, { submitLabel: "Done", onSubmit: closeModal });
}

async function react(target, value) {
  const persona = activePersona(session.state);
  try { await mutate("reaction.set", { persona_id: persona.id, target, value }, value === "0" ? "Current stance reset; vote history retained." : "Stance recorded."); } catch { /* toast already shown */ }
}

function showMessageComposer(recipient = "") {
  const persona = activePersona(session.state);
  const initialRecipient = typeof recipient === "string" ? recipient : "";
  modal("New private message", `Sending as ${persona.displayName} using NIP-17.`, element("div", {}, [
    field("Recipient npub or hex key", "text", "recipient", initialRecipient, "Messages use public persona keys.", { required: true }),
    field("Message", "textarea", "body", "", "Nostr private messaging is interoperable, not promised as an invulnerable high-security messenger.", { required: true }),
  ]), { submitLabel: "Send message", onSubmit: (data) => mutate("message.send", { persona_id: persona.id, recipient: data.get("recipient"), body: data.get("body"), recipient_relays: [] }, "Private message wrapped and queued for the recipient’s inbox relays.") });
}

function showMessageComposerTo(recipient) {
  showMessageComposer(recipient);
}

function showFollowEditor() {
  const persona = activePersona(session.state);
  modal("Follow a persona", "Follows belong to this public persona. Choose whether the relationship is public or privately encrypted.", element("div", {}, [
    field("Persona npub or hex key", "text", "target", "", "Enter a public Nostr persona key.", { required: true }),
    toggle("Publish this follow", "public", true, "Turn this off to keep the follow in this persona’s encrypted local/private list."),
  ]), { submitLabel: "Follow", onSubmit: (data) => mutate("follow.set", { persona_id: persona.id, target: data.get("target"), public: Boolean(data.get("public")), following: true }, "Follow updated for this persona.") });
}

function showPersonaProfile(publicKey) {
  const active = activePersona(session.state);
  const known = (session.state.personas ?? []).find((item) => item.publicKey === publicKey);
  const authored = (session.state.objects ?? []).filter((item) => item.author === publicKey);
  const posts = authored.filter((item) => item.kind === "post");
  const norms = authored.filter((item) => item.kind === "norm");
  const comments = authored.filter((item) => item.kind === "comment");
  const followSets = (session.state.publicFollowSets ?? []).filter((item) => item.personaId === known?.id);
  const alreadyFollowed = (session.state.follows ?? []).some((item) => item.personaId === active.id && item.target === publicKey);
  modal(known?.displayName ?? "Nostr persona", "Public Nostr identity. Private identity and local credential information are not displayed.", element("div", { class: "content-list" }, [
    element("p", { class: "evidence-note", text: publicKey }),
    known?.redditProof ? element("p", { class: "evidence-note", text: `Optional public Reddit proof: ${known.redditProof}` }) : null,
    element("p", { text: `${posts.length} posts · ${comments.length} comments · ${norms.length} norm statements. Counts are secondary context, not a reputation score.` }),
    ...posts.slice(0, 8).map((item) => element("button", { type: "button", class: "text-action", text: item.title || "Untitled discussion", onclick: () => { closeModal(); session.selected = item.anchor; render(); } })),
    ...norms.slice(0, 5).map((item) => element("p", { class: "evidence-note", text: `Norm position: ${item.body}` })),
    ...followSets.map((item) => element("p", { class: "evidence-note", text: `Public follow set: ${item.title} (${item.members.length} selected personas)` })),
    publicKey !== active.publicKey && !alreadyFollowed ? actionButton("Follow this persona", () => { closeModal(); mutate("follow.set", { persona_id: active.id, target: publicKey, public: true, following: true }, "Public follow updated."); }, "primary-button") : null,
    publicKey !== active.publicKey ? instantJudgmentButton("Silence this persona", "silence", publicKey, (event) => queueSilence(event, publicKey), "quiet-button") : null,
    publicKey !== active.publicKey ? instantJudgmentButton("Block this persona", "block", publicKey, (event) => queueBlock(event, publicKey), "danger-button") : null,
    publicKey !== active.publicKey ? actionButton("Message this persona", () => { closeModal(); showMessageComposerTo(publicKey); }) : null,
  ]), { submitLabel: "Close", onSubmit: closeModal });
}

function showFollowSetEditor(existing = null) {
  const persona = activePersona(session.state);
  modal("Publish a curated follow set", "Only the listed public personas are disclosed. Local keyring relationships are not included.", element("div", {}, [
    field("Stable identifier", "text", "identifier", existing?.identifier ?? "recommended", "Used as the NIP-51 addressable list identifier. Keep it stable when revising this set.", { required: true }),
    field("Public title", "text", "title", existing?.title ?? "Recommended personas", "Visible to compatible Nostr clients.", { required: true }),
    field("Persona keys", "textarea", "members", (existing?.members ?? []).join("\n"), "One npub or hex key per line. Publish a deliberately small, affirmative disclosure.", { required: true }),
  ]), { submitLabel: existing ? "Publish revision" : "Publish set", onSubmit: (data) => {
    const members = String(data.get("members") ?? "").split(/[\s,]+/).map((item) => item.trim()).filter(Boolean);
    return mutate("follow_set.publish", { persona_id: persona.id, identifier: data.get("identifier"), title: data.get("title"), members }, "Public NIP-51 follow set queued to this persona’s write relays.");
  } });
}

function showBlockEditor(target = "") {
  const persona = activePersona(session.state);
  const currentTopic = session.route === "community" && session.community ? session.community : null;
  modal("Block a persona", "This changes your view. It does not ban the persona or prevent anyone from reading public content.", element("div", {}, [
    field("Persona npub or hex key", "text", "target", typeof target === "string" ? target : "", "Enter a public Nostr persona key.", { required: true }),
    field("Scope", "select", "scope", currentTopic ? `topic:${currentTopic}` : "global", "A community block applies only within that /h/ topic.", { values: [["global", "Everywhere"], ...(currentTopic ? [[`topic:${currentTopic}`, `/h/${currentTopic}`]] : [])] }),
    toggle("Publish this block", "public", false, "Publishing creates a signed block statement; it does not ban the target."),
    field("Reason", "textarea", "reason", "", "Optional. A private block keeps it encrypted; a published block shares it."),
  ]), { submitLabel: "Block", danger: true, onSubmit: (data) => {
    const scope = String(data.get("scope"));
    return mutate("block.set", { persona_id: persona.id, target: data.get("target"), public: Boolean(data.get("public")), blocked: true, action: "block", topic: scope.startsWith("topic:") ? scope.slice(6) : null, reason: data.get("reason") || null }, "Block applied to this persona’s view.");
  } });
}

function showSilenceEditor(target = "") {
  const persona = activePersona(session.state);
  const currentTopic = session.route === "community" && session.community ? session.community : null;
  modal("Silence a persona", "Silence hides this persona’s new activity from now on. Their earlier activity remains visible.", element("div", {}, [
    field("Persona npub or hex key", "text", "target", typeof target === "string" ? target : "", "Enter a public Nostr persona key.", { required: true }),
    field("Scope", "select", "scope", currentTopic ? `topic:${currentTopic}` : "global", "A community silence applies only within that /h/ topic.", { values: [["global", "Everywhere"], ...(currentTopic ? [[`topic:${currentTopic}`, `/h/${currentTopic}`]] : [])] }),
    toggle("Publish this silence", "public", false, "Publishing shares the judgment and its optional reason; it does not mute the target for anyone else."),
    field("Reason", "textarea", "reason", "", "Optional. Keep the silence private if you do not want to share the reason."),
  ]), { submitLabel: "Silence", onSubmit: (data) => {
    const scope = String(data.get("scope"));
    return mutate("silence.set", { persona_id: persona.id, target: data.get("target"), public: Boolean(data.get("public")), silenced: true, action: "silence", topic: scope.startsWith("topic:") ? scope.slice(6) : null, reason: data.get("reason") || null }, "New activity from this persona is now silenced.");
  } });
}

function showBlockSourceEditor() {
  const persona = activePersona(session.state);
  const currentTopic = session.route === "community" && session.community ? session.community : null;
  const nextRank = Math.max(0, ...(session.state.blockSources ?? []).filter((item) => item.personaId === persona.id).map((item) => Number(item.rank) || 0)) + 1;
  modal("Follow block judgments", "Choose whose block judgments affect this persona’s view. You can stop following them at any time.", element("div", {}, [
    field("Persona npub or hex key", "text", "source", "", "This is a judgment source, not a moderator appointment.", { required: true }),
    toggle("Apply everywhere", "global", !currentTopic, "Includes all topics unless a more direct judgment applies."),
    field("Community topics", "textarea", "topics", currentTopic ?? "", "Optional; one bare topic per line, such as science."),
    field("Priority", "number", "rank", nextRank, "Lower numbers take precedence when followed sources disagree.", { min: 1, required: true }),
  ]), { submitLabel: "Follow block judgments", onSubmit: (data) => {
    const topics = String(data.get("topics") ?? "").split(/[\s,]+/).map((item) => item.trim()).filter(Boolean);
    if (!Boolean(data.get("global")) && !topics.length) throw new Error("Choose everywhere or at least one community topic.");
    return mutate("block_source.set", { persona_id: persona.id, source: data.get("source"), global: Boolean(data.get("global")), topics, rank: Number(data.get("rank")), enabled: true }, "Block judgments will affect this persona’s view after source sync.");
  } });
}

function showSilenceSourceEditor() {
  const persona = activePersona(session.state);
  const currentTopic = session.route === "community" && session.community ? session.community : null;
  const nextRank = Math.max(0, ...(session.state.silenceSources ?? []).filter((item) => item.personaId === persona.id).map((item) => Number(item.rank) || 0)) + 1;
  modal("Follow silence judgments", "Choose whose silence judgments affect this persona’s view. Earlier activity remains available.", element("div", {}, [
    field("Persona npub or hex key", "text", "source", "", "This follows one person’s judgments; it does not appoint a moderator.", { required: true }),
    toggle("Apply everywhere", "global", !currentTopic, "Includes all topics unless a more direct judgment applies."),
    field("Community topics", "textarea", "topics", currentTopic ?? "", "Optional; one bare topic per line, such as science."),
    field("Priority", "number", "rank", nextRank, "Lower numbers take precedence when followed sources disagree.", { min: 1, required: true }),
  ]), { submitLabel: "Follow silence judgments", onSubmit: (data) => {
    const topics = String(data.get("topics") ?? "").split(/[\s,]+/).map((item) => item.trim()).filter(Boolean);
    if (!Boolean(data.get("global")) && !topics.length) throw new Error("Choose everywhere or at least one community topic.");
    return mutate("silence_source.set", { persona_id: persona.id, source: data.get("source"), global: Boolean(data.get("global")), topics, rank: Number(data.get("rank")), enabled: true }, "Silence judgments will affect this persona’s view after source sync.");
  } });
}

function showContentSourceEditor(kind) {
  const persona = activePersona(session.state);
  const removal = kind === "removal";
  const currentTopic = session.route === "community" && session.community ? session.community : null;
  const sources = removal ? session.state.removalSources : session.state.hideSources;
  const nextRank = Math.max(0, ...(sources ?? []).filter((item) => item.personaId === persona.id).map((item) => Number(item.rank) || 0)) + 1;
  const label = removal ? "removal" : "hide";
  modal(`Follow ${label} judgments`, removal
    ? "Choose whose community-removal judgments affect this persona’s community views."
    : "Choose whose content-hide judgments affect this persona’s view.", element("div", {}, [
    field("Persona npub or hex key", "text", "source", "", "This follows one person’s judgments; it does not appoint a moderator.", { required: true }),
    removal ? null : toggle("Apply everywhere", "global", !currentTopic, "Includes all topics unless a more direct judgment applies."),
    field("Community topics", "textarea", "topics", currentTopic ?? "", removal ? "Required; one bare topic per line." : "Optional; one bare topic per line."),
    field("Priority", "number", "rank", nextRank, "Lower numbers take precedence when followed sources disagree.", { min: 1, required: true }),
  ]), { submitLabel: `Follow ${label} judgments`, onSubmit: (data) => {
    const topics = String(data.get("topics") ?? "").split(/[\s,]+/).map((item) => item.trim()).filter(Boolean);
    const global = !removal && Boolean(data.get("global"));
    if (!global && !topics.length) throw new Error("Choose everywhere or at least one community topic.");
    return mutate(removal ? "removal_source.set" : "hide_source.set", { persona_id: persona.id, source: data.get("source"), global, topics, rank: Number(data.get("rank")), enabled: true }, `${label[0].toUpperCase()}${label.slice(1)} judgments will affect this persona’s view after source sync.`);
  } });
}

function contentSourceRow(item, kind) {
  const persona = activePersona(session.state);
  const removal = kind === "removal";
  const label = removal ? "removals" : "hides";
  return element("div", { class: "readiness-row" }, [
    element("div", {}, [
      element("strong", { text: `${item.source.slice(0, 18)}…` }),
      element("p", { text: `Following ${label} ${[item.global ? "everywhere" : null, ...(item.topics ?? []).map((topic) => `/h/${topic}`)].filter(Boolean).join(", ")} · priority ${item.rank} · ${item.completeness}` }),
    ]),
    actionButton(`Stop following ${label}`, () => mutate(removal ? "removal_source.set" : "hide_source.set", { persona_id: persona.id, source: item.source, global: false, topics: [], rank: item.rank, enabled: false }, `${label[0].toUpperCase()}${label.slice(1)} source removed.`)),
  ]);
}

function showLocalFilterEditor() {
  const persona = activePersona(session.state);
  modal("Add a local filter", "This encrypted filter changes only what this persona sees; raw signed evidence remains available.", element("div", {}, [
    field("Filter kind", "select", "kind", "word", "", { values: [["word", "Word or phrase"], ["topic", "Topic / community"], ["thread", "Thread anchor"], ["media", "Media pattern"], ["relay", "Relay URL"]] }),
    field("Value", "text", "value", "", "Match is local and case-insensitive for words and topics.", { required: true }),
  ]), { submitLabel: "Add filter", onSubmit: (data) => mutate("filter.set", { persona_id: persona.id, kind: data.get("kind"), value: data.get("value"), enabled: true }, "Encrypted local filter added.") });
}

function showBackupExport() {
  const persona = activePersona(session.state);
  if (!desktopDialog) { toast("The desktop file chooser is unavailable.", true); return; }
  modal("Back up this persona", `Create a passphrase-encrypted archive for ${persona.displayName}. The archive is stored only at the selected location.`, element("div", {}, [
    field("Backup passphrase", "password", "passphrase", "", "Use at least 12 characters. Losing it makes the archive unreadable.", { required: true }),
    field("Confirm passphrase", "password", "confirmation", "", "Hydra verifies the encrypted archive before reporting success.", { required: true }),
  ]), { submitLabel: "Choose archive location", onSubmit: async (data) => {
    const passphrase = String(data.get("passphrase"));
    if (passphrase !== data.get("confirmation")) throw new Error("The passphrases do not match.");
    if (passphrase.length < 12) throw new Error("Use at least 12 characters for the backup passphrase.");
    const slug = persona.displayName.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "") || "persona";
    const path = await desktopDialog.save({ defaultPath: `hydra-${slug}.age`, filters: [{ name: "Hydra encrypted archive", extensions: ["age"] }] });
    if (!path) return;
    await mutate("backup.export", { persona_id: persona.id, path, passphrase }, "Encrypted persona archive written and verified.");
  } });
}

async function showBackupRestore() {
  if (!desktopDialog) { toast("The desktop file chooser is unavailable.", true); return; }
  const path = await desktopDialog.open({ multiple: false, directory: false, filters: [{ name: "Hydra encrypted archive", extensions: ["age"] }] });
  if (!path || Array.isArray(path)) return;
  modal("Restore encrypted archive", "Restore is transactional and only available before any local persona exists.", field("Backup passphrase", "password", "passphrase", "", "Hydra verifies the archive before replacing the disposable empty root.", { required: true }), {
    submitLabel: "Restore archive",
    onSubmit: (data) => mutate("backup.restore", { persona_id: null, path, passphrase: data.get("passphrase") }, "Encrypted persona archive restored and verified."),
  });
}

async function connectReddit() {
  const persona = activePersona(session.state);
  try { await mutate("reddit.oauth.connect", { persona_id: persona.id, client_id: null }, "Reddit account linked to this persona."); } catch { /* toast shown */ }
}

async function disconnectReddit() {
  const persona = activePersona(session.state);
  if (!window.confirm("Disconnect this persona’s Reddit projection endpoint? Hydra content and persona history remain intact.")) return;
  await mutate("reddit.oauth.unlink", { persona_id: persona.id }, "Reddit disconnected; Hydra remains available.");
}

function showRedditIdentityProof() {
  const persona = activePersona(session.state);
  const challenge = `Verifying that I control the following Nostr public key: ${persona.publicKey}`;
  modal("Publish a Reddit identity proof", "This is optional. It publicly links this Nostr persona to the currently connected Reddit account.", element("div", {}, [
    field("Exact challenge", "textarea", "challenge", challenge, "Post this exact text in a public Reddit post or comment using the linked account."),
    field("Public Reddit permalink", "url", "artifact_url", persona.redditProof ?? "", "Hydra verifies the author and exact challenge before publishing the NIP-39 claim.", { required: true }),
  ]), { submitLabel: "Verify and publish", onSubmit: (data) => mutate("reddit.identity_proof.publish", { persona_id: persona.id, artifact_url: data.get("artifact_url") }, "Verified Reddit identity proof queued for Nostr publication.") });
}

async function installFirefox() {
  try { await mutate("firefox.install", { open: true }, "Firefox companion prepared."); } catch { /* toast shown */ }
}

async function projectionAction(action, projectionId, message) {
  const payload = action === "reddit.big_stick" ? { projection_id: projectionId, portable_link: null } : { projection_id: projectionId };
  try { await mutate(action, payload, message); } catch { /* toast shown */ }
}

async function resolveProjectionDuplicates(projection) {
  if (!window.confirm("Keep this local mapping and abandon the other Hydra mappings for the same destination? Existing Reddit posts or comments will not be deleted or edited.")) return;
  await mutate("reddit.projection.resolve_duplicates", { keep_projection_id: projection.id }, "Duplicate mappings resolved locally; existing Reddit objects were left untouched.");
}

function showContinuity(post) {
  const projection = session.state.projections.find((item) => item.anchor === post.anchor);
  if (!projection) { toast("No Reddit projection record exists for this item.", true); return; }
  const body = element("div", {}, [
    element("p", { text: "Big Stick archives and verifies before adding an uncensorable-record marker. Reddacted archives and verifies before withdrawing." }),
    actionButton("Attach Big Stick record", () => { closeModal(); showBigStick(projection); }),
    actionButton("Reddact from Reddit", () => { closeModal(); showReddact(projection); }, "danger-button"),
  ]);
  modal("Continuity", "The signed Hydra record is the source; Reddit contains a projected copy.", body, { submitLabel: "Close", onSubmit: closeModal });
}

function archiveLevelValues() {
  return [["item", "Item only"], ["ancestors", "Item + ancestors"], ["visible_siblings", "Item + ancestors + visible siblings"], ["loaded_thread", "Entire loaded thread"]];
}

function showBigStick(projection) {
  const defaultLevel = session.state.settings?.continuity?.big_stick_archive_level ?? "item";
  modal("Attach Big Stick record", "Hydra verifies its own signed source record before adding a portable link to this Hydra-originated Reddit copy.", field("Preservation level", "select", "archive_level", defaultLevel, "Only Hydra-originated content is eligible.", { values: archiveLevelValues() }), {
    submitLabel: "Preserve, verify, and attach",
    onSubmit: (data) => mutate("reddit.big_stick", { projection_id: projection.id, portable_link: null, archive_level: data.get("archive_level") }, "Uncensorable record attached."),
  });
}

function showReddact(projection) {
  const markers = [
    ["reddacted", "[Reddacted — view in Hydra]"],
    ["withdrawn", "[Withdrawn from Reddit — view in Hydra]"],
    ["continues", "[The discussion continues on Hydra]"],
    ["elsewhere", "[Redacted. The discussion continues elsewhere.]"],
  ];
  modal("Reddact this projection", "This permanently withdraws the Hydra-originated Reddit copy after its Hydra record is verified. Hydra does not offer restoration.", element("div", {}, [
    field("Withdrawal marker", "select", "marker", "withdrawn", "This is public withdrawal, not encrypted secrecy.", { values: [...markers, ["custom", "Custom wording"]] }),
    field("Custom wording", "text", "custom", "", "Used only with Custom wording; Hydra appends the portable link."),
    field("Preservation level", "select", "archive_level", session.state.settings?.continuity?.reddacted_archive_level ?? "item", "Only Hydra-originated content is preserved.", { values: archiveLevelValues() }),
  ]), { submitLabel: "Preserve, then withdraw", danger: true, onSubmit: (data) => {
    const marker = data.get("marker") === "custom" ? `custom:${data.get("custom")}` : data.get("marker");
    return mutate("reddit.withdraw", { projection_id: projection.id, portable_link: null, marker, archive_level: data.get("archive_level") }, "Projection withdrawn from Reddit; Hydra continuity preserved.");
  } });
}

async function saveSettings(event) {
  event.preventDefault();
  const data = new FormData(event.currentTarget);
  const relays = String(data.get("relays")).split(/\s+/).map((item) => item.trim()).filter(Boolean);
  const personaReadRelays = String(data.get("persona_read_relays")).split(/\s+/).map((item) => item.trim()).filter(Boolean);
  const personaWriteRelays = String(data.get("persona_write_relays")).split(/\s+/).map((item) => item.trim()).filter(Boolean);
  const inboxRelays = String(data.get("inbox_relays")).split(/\s+/).map((item) => item.trim()).filter(Boolean);
  const settings = session.state.settings ?? {};
  const persona = activePersona(session.state);
  const personaDefaults = applyOverride(settings.persona_crosspost_defaults, persona.id, data.get("persona_crosspost"));
  let contentDefaults = applyOverride(settings.content_crosspost_defaults, "post", data.get("post_crosspost"));
  contentDefaults = applyOverride(contentDefaults, "comment", data.get("comment_crosspost"));
  const blobServers = String(data.get("blob_servers")).split(/\s+/).map((item) => item.trim()).filter(Boolean);
  const personaBlobServers = { ...(settings.persona_blob_servers ?? {}), [persona.id]: blobServers };
  const feedSourceWeights = { followed: Number(data.get("feed_followed")), communities: Number(data.get("feed_communities")), replies: Number(data.get("feed_replies")), revisit: Number(data.get("feed_revisit")) };
  await mutate("settings.update", { relays, persona_id: persona.id, persona_read_relays: personaReadRelays, persona_write_relays: personaWriteRelays, inbox_relays: inboxRelays, replication_threshold: Number(data.get("replication")), theme: data.get("theme"), onboarding_complete: null, spam_filter_threshold: Number(data.get("spam_threshold")), remote_media_policy: data.get("remote_media_policy"), crosspost_default: Boolean(data.get("crosspost")), book_club_cross_links_enabled: session.companions.bookClubInstalled ? Boolean(data.get("book_club_cross_links")) : settings.cross_links?.book_club_enabled !== false, persona_crosspost_defaults: personaDefaults, community_crosspost_defaults: parseCommunityOverrides(data.get("community_crossposts")), content_crosspost_defaults: contentDefaults, media_copy_enabled: Boolean(data.get("media_copy")), max_media_bytes: Number(data.get("max_media_mib")) * 1048576, persona_blob_servers: personaBlobServers, feed_source_weights: feedSourceWeights, big_stick_enabled: Boolean(data.get("big_stick_enabled")), reddacted_enabled: Boolean(data.get("reddacted_enabled")), big_stick_archive_level: data.get("big_stick_archive_level"), reddacted_archive_level: data.get("reddacted_archive_level"), continuity_replication_threshold: Number(data.get("continuity_replication")), preferred_gateway_template: data.get("preferred_gateway") }, "Settings saved locally.");
  if (String(data.get("display_name")).trim() !== persona.displayName) {
    await mutate("persona.profile.update", { persona_id: persona.id, display_name: String(data.get("display_name")).trim() }, "Public persona profile updated and queued for publication.");
  }
}

function showSearchResults(query, result, network = false) {
  const hits = result?.result?.hits ?? result?.hits ?? [];
  const cards = hits.map((hit) => element("article", { class: "context-card" }, [
    element("div", { class: "meta-line" }, [
      element("span", { class: "provenance", text: hit.source === "nostr" ? "Nostr network" : hit.source === "draft" ? "Private draft" : "Hydra local" }),
      hit.sourceAuthor ? element("span", { text: `Source: ${hit.sourceAuthor}` }) : null,
      hit.author ? element("span", { text: `${hit.author.slice(0, 12)}…` }) : null,
    ]),
    hit.title ? element("h3", { text: visibleInlineText(hit.title) }) : null,
    element("p", { class: "post-body", text: hit.body || "No text body" }),
    hit.communities?.length ? element("p", { class: "evidence-note", text: hit.communities.map((item) => `/h/${item}`).join(" · ") }) : null,
  ]));
  const body = element("div", { class: "content-list" }, [
    ...(cards.length ? cards : [element("p", { text: "No matching items were found in this search scope." })]),
    !network ? actionButton("Search selected Nostr relays", async () => {
      try { showSearchResults(query, await runtime("search.network", { query, limit: 50 }), true); } catch (error) { toast(readableError(error), true); }
    }, "primary-button") : null,
  ]);
  modal(network ? "Nostr network search" : "Local search", network ? "Results came from selected relays and remain transient until used." : "Search covers encrypted local data.", body, { submitLabel: "Close", onSubmit: closeModal });
}

async function showRawEvidence() {
  const response = await runtime("events.raw", { limit: 250 });
  const result = response?.result ?? response ?? {};
  const events = result.events ?? [];
  const rows = events.map((item) => {
    const kind = Object.keys(item.event ?? {})[0] ?? "event";
    return element("details", { class: "raw-event" }, [
      element("summary", { text: `${kind} · ${relativeTime(item.recordedAt)}` }),
      element("pre", { text: JSON.stringify(item, null, 2) }),
    ]);
  });
  modal("Raw local evidence", `${events.length} newest of ${result.total ?? events.length} checksum-verified events. Local filters are intentionally not applied here.`, element("div", { class: "content-list" }, rows.length ? rows : [element("p", { text: "The local evidence ledger is empty." })]), { submitLabel: "Close", onSubmit: closeModal });
}

async function preserveMedia(object) {
  if (!desktopDialog) { toast("The desktop file chooser is unavailable.", true); return; }
  const paths = await desktopDialog.open({ multiple: true, directory: false });
  if (!paths) return;
  const selected = Array.isArray(paths) ? paths : [paths];
  const mimeFor = (path) => ({ png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", gif: "image/gif", webp: "image/webp", mp4: "video/mp4", webm: "video/webm", mp3: "audio/mpeg", ogg: "audio/ogg", pdf: "application/pdf" }[String(path).split(".").pop().toLowerCase()] ?? "application/octet-stream");
  setBusy(true);
  try {
    for (const path of selected) await runtime("media.preserve", { object: object.anchor, source_path: path, mime_type: mimeFor(path), original_url: null });
    session.state = extractState(await runtime("state"));
    toast(`${selected.length} media file${selected.length === 1 ? "" : "s"} preserved by content hash.`);
    render();
  } catch (error) {
    toast(readableError(error), true);
  } finally {
    setBusy(false);
  }
}

document.querySelectorAll("[data-nav]").forEach((button) => button.addEventListener("click", () => setRoute(button.dataset.nav)));
document.querySelector("#compose-button").addEventListener("click", () => showComposer(session.community));
document.querySelector("#sync-button").addEventListener("click", async () => {
  try { await mutate("sync.now", {}, "Relay and active Reddit synchronization completed."); } catch { /* toast shown */ }
});
document.querySelector("#persona-button").addEventListener("click", () => activePersona(session.state) ? showPersonaMenu() : showPersonaCreator());
document.querySelector("#add-community").addEventListener("click", () => {
  modal("Open a community", "Hydra communities are ownerless topics without membership approval.", field("Community", "text", "community", "", "Use a bare name or /h/name.", { required: true, placeholder: "science" }), { submitLabel: "Open /h/", onSubmit: (data) => {
    const community = validCommunity(data.get("community"));
    if (!community) throw new Error("Use letters, numbers, or underscores only.");
    closeModal(); setRoute("community", community);
  } });
});
document.querySelector("#global-search").addEventListener("keydown", async (event) => {
  if (event.key !== "Enter") return;
  const query = event.currentTarget.value.trim();
  const redditTarget = parseRedditObjectUrl(query);
  if (redditTarget) { await openRedditObject(redditTarget); return; }
  const community = validCommunity(query);
  if (community && (/^\/(?:h|r)\//i.test(query) || !query.includes(" "))) { setRoute("community", community); return; }
  try { const result = await runtime("search.local", { persona_id: activePersona(session.state)?.id ?? null, query, limit: 50 }); showSearchResults(query, result); } catch (error) { toast(readableError(error), true); }
});
document.addEventListener("keydown", (event) => {
  const editable = event.target instanceof HTMLInputElement || event.target instanceof HTMLTextAreaElement || event.target instanceof HTMLSelectElement || event.target?.isContentEditable;
  if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") { event.preventDefault(); document.querySelector("#global-search").focus(); }
  else if (editable && (event.metaKey || event.ctrlKey || event.altKey)) return;
  if (event.key === "Escape") closeModal();
});
document.addEventListener("visibilitychange", () => {
  if (document.hidden || session.busy || modalRoot.childElementCount) return;
  refresh();
  if (session.reddit.threadRoot) resetRedditThreadRefresh();
});

refresh().then(listenForHydraLinks).catch((error) => toast(readableError(error), true));
