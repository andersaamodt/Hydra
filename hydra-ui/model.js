export const LENSES = [
  ["new", "New"],
  ["old", "Old"],
  ["top", "Top"],
  ["following", "Following"],
  ["trusted", "Trusted"],
  ["discussed", "Discussed"],
  ["controversial", "Controversial"],
  ["revisited", "Revisited"],
  ["recovered", "Recovered"],
];

export function activePersona(state) {
  return state?.personas?.find((persona) => persona.active) ?? null;
}

const DIRECTIONAL_CONTROLS = new Map([
  ["\u061c", "ALM"],
  ["\u200e", "LRM"],
  ["\u200f", "RLM"],
  ["\u202a", "LRE"],
  ["\u202b", "RLE"],
  ["\u202c", "PDF"],
  ["\u202d", "LRO"],
  ["\u202e", "RLO"],
  ["\u2066", "LRI"],
  ["\u2067", "RLI"],
  ["\u2068", "FSI"],
  ["\u2069", "PDI"],
]);

export function visibleInlineText(value) {
  return String(value ?? "").replace(
    /[\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069]/gu,
    (control) => `⟦${DIRECTIONAL_CONTROLS.get(control)}⟧`,
  );
}

export function normalizeCommunity(value) {
  return value.trim().replace(/^\/(?:h|r)\//i, "").toLowerCase();
}

export function validCommunity(value) {
  const normalized = normalizeCommunity(value);
  return /^[a-z0-9_]{1,64}$/.test(normalized) ? normalized : null;
}

export function parseRedditObjectUrl(value) {
  try {
    if (typeof value !== "string" || value.length > 4096) return null;
    const url = new URL(value);
    if (
      url.protocol !== "https:" ||
      !["reddit.com", "www.reddit.com", "old.reddit.com"].includes(url.hostname.toLowerCase()) ||
      url.username ||
      url.password ||
      (url.port && url.port !== "443")
    ) return null;
    const parts = url.pathname.split("/").filter(Boolean);
    if (parts[0]?.toLowerCase() !== "r" || parts[2]?.toLowerCase() !== "comments") return null;
    const community = validCommunity(parts[1] ?? "");
    const postId = /^[a-z0-9]{1,32}$/i.test(parts[3] ?? "") ? parts[3].toLowerCase() : null;
    const commentId = /^[a-z0-9]{1,32}$/i.test(parts[5] ?? "") ? parts[5].toLowerCase() : null;
    if (!community || !postId) return null;
    return { community, postFullname: `t3_${postId}`, commentFullname: commentId ? `t1_${commentId}` : null };
  } catch {
    return null;
  }
}

export function commentsFor(state, root) {
  const maximumItems = 2000;
  const maximumDepth = 64;
  const all = state?.objects ?? [];
  const visible = new Set(state?.visibleAnchors ?? all.map((object) => object.anchor));
  const children = new Map();
  for (const object of all) {
    if (object.kind !== "comment" || object.root !== root || !visible.has(object.anchor)) continue;
    const parent = object.parent ?? root;
    const values = children.get(parent) ?? [];
    values.push(object);
    children.set(parent, values);
  }
  for (const values of children.values()) values.sort((a, b) => a.editedAt - b.editedAt);
  const pending = [...(children.get(root) ?? [])].reverse().map((item) => ({ item, depth: 0 }));
  const seen = new Set([root]);
  const output = [];
  while (pending.length && output.length < maximumItems) {
    const { item, depth } = pending.pop();
    if (seen.has(item.anchor)) continue;
    seen.add(item.anchor);
    output.push({ ...item, depth });
    if (depth >= maximumDepth) continue;
    const descendants = children.get(item.anchor) ?? [];
    for (let index = descendants.length - 1; index >= 0; index -= 1) {
      pending.push({ item: descendants[index], depth: depth + 1 });
    }
  }
  return output;
}

export function sortedPosts(state, lens = "new", community = null) {
  const objects = new Map((state?.objects ?? []).map((object) => [object.anchor, object]));
  const order = state?.feedOrders?.[lens] ?? [];
  return order
    .map((anchor) => objects.get(anchor))
    .filter((object) => object?.kind === "post" && (!community || object.communities?.includes(community)));
}

export function myFeedPosts(state, posts) {
  const byAnchor = new Map(posts.map((object) => [object.anchor, object]));
  return (state?.myFeedOrder ?? []).map((anchor) => byAnchor.get(anchor)).filter(Boolean);
}

export function relativeTime(timestamp, now = Math.floor(Date.now() / 1000)) {
  if (!Number.isFinite(timestamp)) return "unknown time";
  const seconds = Math.max(0, now - timestamp);
  if (seconds < 60) return "now";
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h`;
  if (seconds < 2_592_000) return `${Math.floor(seconds / 86400)}d`;
  if (seconds < 31_536_000) return `${Math.floor(seconds / 2_592_000)}mo`;
  return `${Math.floor(seconds / 31_536_000)}y`;
}

export function provenance(object) {
  if (object.redditProjected) return { label: "Hydra → Reddit", tone: "projected" };
  return { label: "Hydra native", tone: "native" };
}

export function redditDepth(item, items) {
  const byId = new Map(items.map((candidate) => [candidate.fullname, candidate]));
  const visited = new Set([item.fullname]);
  let parent = item.parent;
  let depth = 0;
  while (parent && byId.has(parent) && !visited.has(parent) && depth < 6) {
    visited.add(parent);
    depth += 1;
    parent = byId.get(parent).parent;
  }
  return depth;
}

export function discussionItemMatches(origin, subreddits, filter) {
  const originMatches = filter.origin === "all" || filter.origin === origin;
  const subredditMatches = filter.subreddit === "all" || subreddits.includes(filter.subreddit);
  return originMatches && subredditMatches;
}

export function isRedditDiscussionProjection(projection) {
  return ["reddit-community", "reddit-parent"].includes(projection?.destinationSystem)
    && projection?.state !== "withdrawn";
}

export function durabilityLabel(value) {
  return ({
    local: "Local only",
    published: "Published",
    replicated: "Replicated",
    partial: "Partially published",
  })[value] ?? value ?? "Local only";
}

export function whyShown(object, lens, community) {
  if (community) return `Tagged /h/${community}`;
  if (lens === "top") return `Current Hydra score ${object.currentScore ?? 0}`;
  if (lens === "discussed") return `${object.discussionCount ?? 0} replies`;
  if (lens === "controversial") return `Balanced positive and negative reactions`;
  if (lens === "revisited") return "Saved for Revisit";
  if (lens === "recovered") return "Recovered from an external source";
  return "Recent activity";
}
