import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const styles = await readFile(new URL("../styles.css", import.meta.url), "utf8");
const app = await readFile(new URL("../app.js", import.meta.url), "utf8");
const index = await readFile(new URL("../index.html", import.meta.url), "utf8");
const tauri = await readFile(new URL("../../apps/desktop/tauri/tauri.conf.json", import.meta.url), "utf8");

test("light mode supplies semantic surfaces instead of inheriting dark literals", () => {
  for (const variable of ["--nav-ink", "--bar", "--card-hover", "--body-ink", "--modal"]) {
    assert.match(styles, new RegExp(`:root\\[data-theme=\"light\"\\][\\s\\S]*${variable}:`));
    assert.match(styles, new RegExp(`:root\\[data-theme=\"system\"\\][\\s\\S]*${variable}:`));
  }

  assert.match(styles, /\.post-card\s*\{[^}]*background:\s*transparent/);
  assert.match(styles, /\.context-card\s*\{[^}]*border-bottom:\s*1px solid var\(--line\)/);
  assert.match(styles, /\.lens-bar\s*\{[^}]*background:\s*var\(--bar\)/);
  assert.match(styles, /\.modal\s*\{[^}]*background:\s*var\(--modal\)/);
});

test("the permanent shell avoids AI-generated interface foibles", () => {
  assert.match(index, /<title>Hydra<\/title>/);
  assert.match(tauri, /"title": "Hydra"/);
  assert.doesNotMatch(app, /Your living memory|No rulers, only lenses|view-kicker|view-subtitle|empty-mark/);
  assert.match(app, /function viewHeader\(title, extras = \[\]\)/);
  assert.doesNotMatch(styles, /radial-gradient|linear-gradient|backdrop-filter|border-radius:\s*999px/);
  assert.match(app, /function settingsGroup\(title, children, open = false\)/);
  assert.match(app, /settingsGroup\("Advanced settings"/);
  assert.equal((app.match(/settingsGroup\(/g) ?? []).length, 2);
  assert.match(app, /if \(session\.openNostr\.loaded\) surfaces\.push\(controls\)/);
});

test("selection and conversation state use whole-element treatments", () => {
  const activeNav = styles.match(/\.nav-item\.is-active\s*\{([^}]*)\}/)?.[1] ?? "";
  const comment = styles.match(/\.comment\s*\{([^}]*)\}/)?.[1] ?? "";
  const norm = styles.match(/\.norm-card\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(activeNav, /background:/);
  assert.doesNotMatch(activeNav, /border-left|inset/);
  assert.doesNotMatch(comment, /border-left/);
  assert.doesNotMatch(norm, /border-left/);
});

test("Open Nostr uses a readable one-column card and bounded first page", () => {
  assert.match(styles, /\.post-card\.open-nostr-card\s*\{[^}]*grid-template-columns:\s*minmax\(0,1fr\)/);
  assert.match(app, /filter:\s*"all"/);
  assert.match(app, /\["tagged",\s*"Tagged"\]/);
  assert.match(app, /\["uncategorized",\s*"Uncategorized"\]/);
  assert.match(app, /limit:\s*30/);
});

test("Book Club cross-links require both local consent and an installed handler", () => {
  assert.match(app, /Show Book Club cross-links/);
  assert.match(app, /bookClubCrossLinksAvailable\(\) && item\.bookClubUrl/);
  assert.match(app, /session\.companions\.bookClubInstalled/);
  assert.match(app, /cross_links\?\.book_club_enabled !== false/);
  assert.match(app, /disabled:\s*!session\.companions\.bookClubInstalled/);
});

test("the Reddit Bridge exposes imported posts and comments with exact source links", () => {
  assert.match(app, /Imported Reddit writing/);
  assert.match(app, /item\.externalSource/);
  assert.match(app, /Copy source link/);
  assert.match(app, /slice\(0,\s*25\)/);
  assert.match(styles, /\.source-link\s*\{[^}]*overflow-wrap:\s*anywhere/);
});

test("startup uses the real icon and one atomic splash handoff", () => {
  assert.match(index, /id="boot-splash"[\s\S]*src="hydra-icon\.png"/);
  assert.match(index, /id="app"[\s\S]*hidden/);
  assert.match(app, /function finishBoot\(\)/);
  assert.match(app, /splash\?\.remove\(\)/);
  assert.doesNotMatch(styles, /text-transform:\s*uppercase/);
  assert.doesNotMatch(styles, /button[^{]*:[^{]*\{[^}]*transform:/);
});

test("themes and chamber tabs honor desktop input contracts", () => {
  assert.match(app, /function saveThemeChoice\(event\)/);
  assert.match(app, /runtime\("settings\.update", \{ theme: selected \}\)/);
  assert.match(app, /role:\s*"tab"/);
  assert.match(app, /"ArrowLeft",\s*"ArrowRight"/);
  assert.match(app, /tabindex:\s*session\.chamber/);
  assert.match(styles, /\.discussion-toolbar\s*\{[^}]*flex-wrap:\s*wrap/s);
});

test("Settings explains and opens Hydra's actual local storage", () => {
  assert.match(app, /Local data storage/);
  assert.match(app, /encrypted event log—not as loose Markdown files/);
  assert.match(app, /Open Hydra data folder/);
  assert.match(app, /storage\.mediaExists \? actionButton\("Open preserved media folder"/);
  assert.match(app, /runtime\("storage\.open", \{ folder \}\)/);
  assert.match(app, /Posts are not stored in separate persona folders/);
});

test("interface copy stays functional instead of adopting a persona", () => {
  const interfaceCopy = `${index}\n${tauri}\n${app}`;
  for (const phrase of [
    "I’m with the banned",
    "Your feed is quiet",
    "in a good way",
    "Why this?",
    "Categorize for me",
    "Hydra is safe",
    "Hydra reply is safe",
    "authoritative will of the network",
    "temporal recognition rather than global karma",
    "cannot honestly claim",
    "No one grants membership",
  ]) {
    assert.equal(interfaceCopy.includes(phrase), false, phrase);
  }
  assert.match(app, /No posts in My Feed/);
  assert.match(app, /Feed reason/);
  assert.match(app, /Block locally/);
});

test("background Reddit refresh cannot overwrite a newer interaction", () => {
  assert.match(app, /epoch !== session\.reddit\.requestEpoch/);
  assert.match(app, /session\.busy \|\| modalRoot\.childElementCount \|\| document\.hidden/);
  assert.match(app, /document\.addEventListener\("visibilitychange"/);
});

test("one-click judgments use a pausable anchored grace-period callout", () => {
  assert.match(app, /JUDGMENT_GRACE_MS/);
  assert.match(app, /pendingJudgmentDecision/);
  assert.match(app, /onpointerenter:.*pausePendingJudgment/s);
  assert.match(app, /onpointerleave:.*resumePendingJudgment/s);
  assert.match(app, /dismissPendingJudgmentCallout/);
  assert.match(app, /pending-judgment-recall/);
  assert.match(styles, /\.judgment-callout::before/);
  assert.match(styles, /@keyframes judgment-callout-arrive/);
  assert.doesNotMatch(app, /text: "Hide…"/);
  assert.match(app, /pendingHide.*effect\?\.pending/s);
  assert.match(styles, /\.is-pending-hide/);
  assert.match(styles, /transition: opacity 2\.4s ease, filter 2\.4s ease/);
  assert.match(styles, /judgment-callout-arrive \.8s \.18s/);
  assert.match(app, /class: "judgment-callout-scope"/);
  assert.doesNotMatch(app, /Timer paused while you decide/);
  assert.doesNotMatch(app, /Apply now/);
  assert.match(app, /class: "icon-button judgment-undo".*"aria-label": "Undo"/);
});

test("community routes replace app branding with the bare topic identity", () => {
  assert.match(app, /function renderBrand\(\)/);
  assert.match(app, /domain\.textContent = "\/h\/"/);
  assert.match(app, /name\.textContent = community/);
  assert.match(app, /topicIdenticon\(community\)/);
  assert.match(app, /communityAppearances/);
  assert.match(app, /showCommunityAppearanceEditor/);
  assert.match(app, /community_appearance\.set/);
  assert.match(app, /appearance_source\.set/);
  assert.match(app, /Follow their community images/);
  assert.match(app, /function showPersonaProfile/);
  assert.doesNotMatch(app, /function showAppearanceSources/);
  assert.doesNotMatch(app, /field\("SHA-256"/);
  assert.match(styles, /\.community-image-preview/);
});
