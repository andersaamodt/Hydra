import test from "node:test";
import assert from "node:assert/strict";
import {
  activePersona,
  commentsFor,
  discussionItemMatches,
  isRedditDiscussionProjection,
  JUDGMENT_GRACE_MS,
  parseRedditObjectUrl,
  pendingJudgmentDecision,
  myFeedPosts,
  normalizeCommunity,
  redditDepth,
  relativeTime,
  sortedPosts,
  validCommunity,
  visibleInlineText,
} from "../model.js";

test("optimistic judgments preserve category, target, scope, and silence time", () => {
  const object = { anchor: "note", author: "npub-alice", editedAt: 100 };
  const hide = { kind: "hide", targetType: "anchor", target: "note", topic: "science", excludes: true };
  assert.equal(JUDGMENT_GRACE_MS, 12_000);
  assert.equal(pendingJudgmentDecision(hide, "hide", object, "science"), "exclude");
  assert.equal(pendingJudgmentDecision(hide, "hide", object, "history"), null);
  assert.equal(pendingJudgmentDecision({ ...hide, excludes: false }, "hide", object, "science"), "allow");
  assert.equal(pendingJudgmentDecision({ kind: "block", targetType: "author", target: "npub-alice", excludes: true }, "block", object, null), "exclude");
  assert.equal(pendingJudgmentDecision({ kind: "silence", targetType: "author", target: "npub-alice", excludes: true, cutoff: 101 }, "silence", object, null), null);
  assert.equal(pendingJudgmentDecision({ kind: "silence", targetType: "author", target: "npub-alice", excludes: true, cutoff: 100 }, "silence", object, null), "exclude");
});

test("community paths normalize without collapsing namespaces", () => {
  assert.equal(normalizeCommunity(" /H/Science "), "science");
  assert.equal(normalizeCommunity("/r/Seattle"), "seattle");
  assert.equal(validCommunity("open_science"), "open_science");
  assert.equal(validCommunity("not valid"), null);
});

test("community names keep Reddit-compatible constraints", () => {
  assert.equal(validCommunity("hydra_qa"), "hydra_qa");
  assert.equal(validCommunity("hydra-qa"), null);
});

test("external directional controls are visible without changing stored evidence", () => {
  assert.equal(visibleInlineText("Hydra \u202eardyH"), "Hydra ⟦RLO⟧ardyH");
  assert.equal(visibleInlineText("هايدرا"), "هايدرا");
});

test("Reddit discussion projections include post and exact-parent destinations", () => {
  assert.equal(isRedditDiscussionProjection({ destinationSystem: "reddit-community", state: "live" }), true);
  assert.equal(isRedditDiscussionProjection({ destinationSystem: "reddit-parent", state: "queued" }), true);
  assert.equal(isRedditDiscussionProjection({ destinationSystem: "unrelated-system", state: "live" }), false);
  assert.equal(isRedditDiscussionProjection({ destinationSystem: "reddit-community", state: "withdrawn" }), false);
});

test("Reddit URLs preserve exact post and comment targets", () => {
  assert.deepEqual(
    parseRedditObjectUrl("https://www.reddit.com/r/Science/comments/AbC123/title/DeF456/?context=3"),
    { community: "science", postFullname: "t3_abc123", commentFullname: "t1_def456" },
  );
  assert.deepEqual(
    parseRedditObjectUrl("https://reddit.com/r/science/comments/abc123/title/"),
    { community: "science", postFullname: "t3_abc123", commentFullname: null },
  );
  assert.equal(parseRedditObjectUrl("https://example.com/r/science/comments/abc/title/"), null);
  assert.equal(parseRedditObjectUrl("http://www.reddit.com/r/science/comments/abc/title/"), null);
  assert.equal(parseRedditObjectUrl("https://evil.reddit.com/r/science/comments/abc/title/"), null);
  assert.equal(parseRedditObjectUrl("https://attacker@www.reddit.com/r/science/comments/abc/title/"), null);
  assert.equal(parseRedditObjectUrl("https://www.reddit.com:444/r/science/comments/abc/title/"), null);
  assert.equal(parseRedditObjectUrl(`https://www.reddit.com/r/science/comments/${"a".repeat(33)}/title/`), null);
});

test("active persona remains an explicit public persona", () => {
  const state = { personas: [{ id: "one", active: false }, { id: "two", active: true }] };
  assert.equal(activePersona(state).id, "two");
});

test("one post keeps one nested comment tree", () => {
  const state = { objects: [
    { anchor: "a", kind: "post" },
    { anchor: "c1", kind: "comment", root: "a", parent: "a", editedAt: 1 },
    { anchor: "c2", kind: "comment", root: "a", parent: "c1", editedAt: 2 },
  ] };
  assert.deepEqual(commentsFor(state, "a").map(({ anchor, depth }) => [anchor, depth]), [["c1", 0], ["c2", 1]]);
});

test("hostile comment cycles and widths stay bounded", () => {
  const cyclic = {
    objects: [
      { anchor: "a", kind: "post" },
      { anchor: "c1", kind: "comment", root: "a", parent: "a", editedAt: 1 },
      { anchor: "c2", kind: "comment", root: "a", parent: "c1", editedAt: 2 },
      { anchor: "c1", kind: "comment", root: "a", parent: "c2", editedAt: 3 },
    ],
  };
  assert.deepEqual(commentsFor(cyclic, "a").map(({ anchor }) => anchor), ["c1", "c2"]);

  const wide = {
    objects: [
      { anchor: "a", kind: "post" },
      ...Array.from({ length: 2100 }, (_, index) => ({
        anchor: `c${index}`,
        kind: "comment",
        root: "a",
        parent: "a",
        editedAt: index,
      })),
    ],
  };
  assert.equal(commentsFor(wide, "a").length, 2000);
});

test("live Reddit comments retain their returned nesting", () => {
  const items = [
    { fullname: "t3_root", parent: null },
    { fullname: "t1_parent", parent: "t3_root" },
    { fullname: "t1_child", parent: "t1_parent" },
  ];
  assert.equal(redditDepth(items[0], items), 0);
  assert.equal(redditDepth(items[1], items), 1);
  assert.equal(redditDepth(items[2], items), 2);
});

test("merged discussion filters distinguish origin and source subreddit", () => {
  assert.equal(discussionItemMatches("reddit", ["science"], { origin: "all", subreddit: "science" }), true);
  assert.equal(discussionItemMatches("reddit", ["science"], { origin: "hydra", subreddit: "science" }), false);
  assert.equal(discussionItemMatches("hydra", ["biology"], { origin: "hydra", subreddit: "science" }), false);
  assert.equal(discussionItemMatches("hydra", ["biology"], { origin: "hydra", subreddit: "all" }), true);
});

test("the UI renders only runtime-approved visible objects", () => {
  const state = {
    visibleAnchors: ["visible"],
    feedOrders: { new: ["visible"] },
    objects: [
      { anchor: "visible", kind: "post", author: "npub-friend", editedAt: 1 },
      { anchor: "hidden", kind: "post", author: "npub-blocked", editedAt: 2 },
      { anchor: "c1", kind: "comment", author: "npub-blocked", root: "visible", parent: "visible", editedAt: 3 },
    ],
  };
  assert.deepEqual(sortedPosts(state).map((item) => item.anchor), ["visible"]);
  assert.deepEqual(commentsFor(state, "visible"), []);
});

test("judgment-excluded comments remain available for reversible placeholders", () => {
  const state = {
    visibleAnchors: ["post"],
    objects: [
      { anchor: "post", kind: "post" },
      { anchor: "blocked", kind: "comment", root: "post", parent: "post", editedAt: 2, block: { inherited: false } },
      { anchor: "silenced", kind: "comment", root: "post", parent: "post", editedAt: 3, silence: { cutoff: 2 } },
      { anchor: "hidden", kind: "comment", root: "post", parent: "post", editedAt: 4, hide: { inherited: false } },
      { anchor: "removed", kind: "comment", root: "post", parent: "post", editedAt: 5, topicRemovals: { science: { inherited: true } } },
    ],
  };
  assert.deepEqual(commentsFor(state, "post").map(({ anchor }) => anchor), ["blocked", "silenced", "hidden", "removed"]);
});

test("the UI preserves runtime lens order without recreating policy", () => {
  const state = { feedOrders: {
    new: ["b", "a"],
    top: ["a", "b"],
    discussed: ["b", "a"],
  }, objects: [
    { anchor: "a", kind: "post", editedAt: 1, currentScore: 5, discussionCount: 0, controversy: 0 },
    { anchor: "b", kind: "post", editedAt: 2, currentScore: 1, discussionCount: 8, controversy: 2 },
  ] };
  assert.equal(sortedPosts(state, "new")[0].anchor, "b");
  assert.equal(sortedPosts(state, "top")[0].anchor, "a");
  assert.equal(sortedPosts(state, "discussed")[0].anchor, "b");
});

test("norm statements stay out of runtime-provided post feeds", () => {
  const state = { feedOrders: { new: ["post", "norm"] }, objects: [
    { anchor: "post", kind: "post", editedAt: 2, communities: ["science"] },
    { anchor: "norm", kind: "norm", editedAt: 3, communities: ["science"] },
  ] };
  assert.deepEqual(sortedPosts(state, "new", "science").map((item) => item.anchor), ["post"]);
});

test("My Feed combines weighted people, places, replies, and memory sources", () => {
  const state = {
    myFeedOrder: ["friend", "replied", "own", "topic", "remembered"],
  };
  const posts = [
    { anchor: "own", author: "npub-me", communities: ["elsewhere"] },
    { anchor: "friend", author: "npub-friend", communities: ["elsewhere"] },
    { anchor: "topic", author: "npub-stranger", communities: ["science"] },
    { anchor: "replied", author: "npub-stranger", communities: ["elsewhere"] },
    { anchor: "remembered", author: "npub-stranger", communities: ["elsewhere"] },
    { anchor: "other", author: "npub-stranger", communities: ["elsewhere"] },
  ];
  assert.deepEqual(myFeedPosts(state, posts).map((item) => item.anchor), ["friend", "replied", "own", "topic", "remembered"]);
});

test("relative time is compact and stable", () => {
  assert.equal(relativeTime(900, 1000), "1m");
  assert.equal(relativeTime(1000, 1000), "now");
});
