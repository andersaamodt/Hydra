import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const styles = await readFile(new URL("../styles.css", import.meta.url), "utf8");
const app = await readFile(new URL("../app.js", import.meta.url), "utf8");
const index = await readFile(new URL("../index.html", import.meta.url), "utf8");
const tauri = await readFile(new URL("../../apps/desktop/tauri/tauri.conf.json", import.meta.url), "utf8");
const desktop = await readFile(new URL("../../apps/desktop/tauri/src/main.rs", import.meta.url), "utf8");
const capability = await readFile(new URL("../../apps/desktop/tauri/capabilities/default.json", import.meta.url), "utf8");

test("the default light stone-blue scheme uses derived semantic surfaces", () => {
  for (const variable of ["--accent-seed", "--canvas", "--sidebar-surface", "--bar", "--card-hover", "--modal"]) {
    assert.match(styles, new RegExp(`${variable}:`));
  }

  assert.match(styles, /--accent-seed:\s*#6f8299/);
  assert.match(styles, /--canvas:\s*color-mix\(in srgb, var\(--accent-seed\)/);
  assert.match(styles, /:root\[data-resolved-theme="dark"\]/);
  assert.match(styles, /body\s*\{[^}]*background:\s*var\(--canvas\)/);
  assert.match(styles, /\.topbar\s*\{[^}]*background:\s*var\(--chrome\)/);
  assert.match(styles, /\.post-card\s*\{[^}]*background:\s*transparent/);
  assert.match(styles, /\.context-card\s*\{[^}]*border-bottom:\s*1px solid var\(--line\)/);
  assert.match(styles, /\.lens-bar\s*\{[^}]*background:\s*var\(--bar\)/);
  assert.match(styles, /\.modal\s*\{[^}]*background:\s*var\(--modal\)/);
});

test("dark mode keeps every major surface on the dark palette", () => {
  for (const [selector, token] of [
    ["\\.topbar", "--chrome"],
    ["\\.sidebar", "--sidebar-surface"],
    ["\\.main-panel", "--canvas"],
    ["\\.view-header", "--chrome"],
    ["\\.lens-bar", "--bar"],
    ["\\.settings-tabs", "--panel"],
    ["\\.settings-actions", "--panel"],
    ["\\.modal", "--modal"],
  ]) {
    assert.match(styles, new RegExp(`${selector}\\s*\\{[^}]*background:\\s*var\\(${token}\\)`));
  }

  assert.match(styles, /--paper:\s*var\(--panel\)/);
  assert.match(styles, /:root\[data-resolved-theme="dark"\][\s\S]*--on-accent:\s*#18212b/);
  assert.match(styles, /\.primary-button\s*\{[^}]*color:\s*var\(--on-accent\)/);
});

test("the application shell has no permanent right status sidebar", () => {
  assert.doesNotMatch(index, /context-panel|Context and activity/);
  assert.doesNotMatch(app, /contextPanel|renderContext/);
  assert.doesNotMatch(styles, /\.context-panel|\.context-status|\.status-dot/);
  assert.match(styles, /\.app-shell\s*\{[^}]*grid-template-columns:\s*230px minmax\(480px, 1fr\)/);
});

test("dark text and controls remain legible for every accent", () => {
  const accents = ["#6f8299", "#6574a8", "#826fa3", "#a56f5d", "#6f846f"];
  const rgb = (hex) => [1, 3, 5].map((index) => Number.parseInt(hex.slice(index, index + 2), 16));
  const mix = (foreground, amount, background) => rgb(foreground).map((channel, index) => Math.round(channel * amount + rgb(background)[index] * (1 - amount)));
  const luminance = (color) => {
    const [red, green, blue] = color.map((channel) => channel / 255).map((channel) => channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4);
    return 0.2126 * red + 0.7152 * green + 0.0722 * blue;
  };
  const contrast = (foreground, background) => {
    const [light, dark] = [luminance(foreground), luminance(background)].sort((left, right) => right - left);
    return (light + 0.05) / (dark + 0.05);
  };

  for (const accent of accents) {
    const panel = mix(accent, 0.08, "#151b21");
    const accentStrong = mix(accent, 0.58, "#e5edf5");
    assert.ok(contrast(rgb("#edf1f5"), panel) >= 7, `${accent} primary text`);
    assert.ok(contrast(rgb("#9ca9b7"), panel) >= 4.5, `${accent} secondary text`);
    assert.ok(contrast(rgb("#8796a5"), panel) >= 4.5, `${accent} faint text`);
    assert.ok(contrast(rgb("#18212b"), accentStrong) >= 4.5, `${accent} button text`);
  }
});

test("the permanent shell avoids AI-generated interface foibles", () => {
  assert.match(index, /<title>Hydra<\/title>/);
  assert.match(tauri, /"title": "Hydra"/);
  assert.doesNotMatch(app, /Your living memory|No rulers, only lenses|view-kicker|view-subtitle|empty-mark/);
  assert.match(app, /function viewHeader\(title, extras = \[\]\)/);
  assert.doesNotMatch(styles, /radial-gradient|linear-gradient|backdrop-filter|border-radius:\s*999px/);
  assert.doesNotMatch(app, /Advanced settings/);
  assert.match(app, /function settingsPane\(id, children\)/);
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
  assert.match(app, /placeholder:\s*"Filter this relay sample"/);
  assert.match(app, /\["all",\s*"All kinds"\]/);
  assert.match(app, /\["hour",\s*"Last hour"\]/);
  assert.match(app, /open-nostr-result-count/);
  assert.match(styles, /\.open-nostr-filter-controls\s*\{[^}]*flex-wrap:\s*wrap/);
});

test("Book Club cross-links require both local consent and an installed handler", () => {
  assert.match(app, /Show Book Club cross-links/);
  assert.match(app, /bookClubCrossLinksAvailable\(\) && item\.bookClubUrl/);
  assert.match(app, /session\.companions\.bookClubInstalled/);
  assert.match(app, /cross_links\?\.book_club_enabled !== false/);
  assert.match(app, /disabled:\s*!session\.companions\.bookClubInstalled/);
});

test("the Reddit Bridge exposes imported posts and comments with exact source links", () => {
  assert.match(app, /function redditBridgeSections\(\)/);
  assert.match(app, /settingsPane\("reddit", \[[\s\S]*\.\.\.redditBridgeSections\(\)/);
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

test("appearance and chamber tabs honor desktop input contracts", () => {
  assert.match(app, /function saveAppearanceChoice\(event\)/);
  assert.match(app, /runtime\("settings\.update", selected\)/);
  assert.match(app, /"stone-blue": "#6f8299"/);
  assert.match(app, /Hydra derives selection, focus, and lightly tinted surfaces from this one color/);
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

test("macOS Settings opens in one dedicated window with horizontal keyboard tabs", () => {
  assert.match(desktop, /fn open_settings_window\(app: AppHandle, tab: Option<String>\)/);
  assert.match(desktop, /WebviewWindowBuilder::new\([\s\S]*"settings"[\s\S]*index\.html\?window=settings/);
  assert.match(desktop, /get_webview_window\("settings"\)/);
  assert.match(desktop, /emit\("settings-tab", &tab\)/);
  assert.match(capability, /"windows": \["main", "settings"\]/);
  assert.match(app, /const SETTINGS_TABS = \[/);
  assert.match(app, /role: "tablist"/);
  assert.match(app, /role: "tabpanel"/);
  assert.match(app, /\["ArrowLeft", "ArrowRight", "Home", "End"\]/);
  assert.match(app, /event\.metaKey && event\.key === ","/);
  assert.match(styles, /\.settings-tabs\s*\{[^}]*display:\s*flex/);
  assert.match(styles, /\.settings-window \.sidebar/);
});

test("secondary destinations leave the sidebar and Messages uses an orangered toolbar envelope", () => {
  const sidebar = index.match(/<aside class="sidebar"[\s\S]*?<\/aside>/)?.[0] ?? "";
  assert.doesNotMatch(sidebar, /Messages|Reddit Bridge|Settings/);
  assert.match(index, /<div class="top-actions">[\s\S]*id="messages-button"[\s\S]*id="settings-button"/);
  assert.match(index, /id="message-badge" class="message-count" hidden/);
  assert.match(app, /messageRequestCount \?\? 0/);
  assert.match(app, /`Messages, \$\{unreadCount\} unread`/);
  assert.match(styles, /\.message-button \.toolbar-icon-glyph\s*\{[^}]*color:\s*#ff4500/);
  assert.match(styles, /\.message-count\s*\{[^}]*background:\s*#ff4500/);
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

test("shared judgments are selected from profiles and remain inspectable", () => {
  assert.match(app, /Use their judgments/);
  assert.match(app, /follow_source\.set/);
  assert.match(app, /pin_source\.set/);
  assert.match(app, /reverse_source\.set/);
  assert.match(app, /function renderCommunityPins/);
  assert.match(app, /pin_dismissal\.set/);
  assert.match(app, /People worth a second look/);
  assert.match(app, /Nothing here follows or unblocks anyone automatically/);
  assert.match(app, /"rescue"/);
  assert.match(styles, /\.pinned-area/);
});
