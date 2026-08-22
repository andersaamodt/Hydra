import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const styles = await readFile(new URL("../styles.css", import.meta.url), "utf8");
const app = await readFile(new URL("../app.js", import.meta.url), "utf8");
const index = await readFile(new URL("../index.html", import.meta.url), "utf8");
const tauri = await readFile(new URL("../../apps/desktop/tauri/tauri.conf.json", import.meta.url), "utf8");
const desktop = await readFile(new URL("../../apps/desktop/tauri/src/main.rs", import.meta.url), "utf8");
const capability = await readFile(new URL("../../apps/desktop/tauri/capabilities/default.json", import.meta.url), "utf8");

test("the default light-blue scheme uses visibly tinted semantic surfaces", () => {
  for (const variable of ["--accent-seed", "--canvas", "--sidebar-surface", "--bar", "--card-hover", "--modal"]) {
    assert.match(styles, new RegExp(`${variable}:`));
  }

  assert.match(styles, /--accent-seed:\s*#5687bb/);
  assert.match(styles, /--canvas:\s*color-mix\(in srgb, var\(--accent-seed\)/);
  assert.match(styles, /--canvas:\s*color-mix\(in srgb, var\(--accent-seed\) 16%/);
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
  assert.match(styles, /\.app-shell\s*\{[^}]*--sidebar-width:\s*230px;[^}]*grid-template-columns:\s*var\(--sidebar-width\) minmax\(480px, 1fr\)/);
});

test("the navigation sidebar can be resized from its right edge", () => {
  assert.match(index, /id="sidebar-resizer"[\s\S]*?role="separator"[\s\S]*?aria-orientation="vertical"[\s\S]*?tabindex="0"/);
  assert.match(styles, /\.sidebar-resizer\s*\{[^}]*left:\s*calc\(var\(--sidebar-width\) - 4px\);[^}]*cursor:\s*col-resize;[^}]*touch-action:\s*none/);
  assert.match(app, /SIDEBAR_WIDTH_MIN\s*=\s*180/);
  assert.match(app, /SIDEBAR_WIDTH_MAX\s*=\s*420/);
  assert.match(app, /localStorage\.setItem\(SIDEBAR_WIDTH_STORAGE_KEY/);
  assert.match(app, /setPointerCapture\(event\.pointerId\)/);
  assert.match(app, /ArrowLeft:[\s\S]*ArrowRight:[\s\S]*Home:[\s\S]*End:/);
  assert.match(app, /addEventListener\("dblclick"[\s\S]*SIDEBAR_WIDTH_DEFAULT/);
});

test("macOS integrates the native title bar with Hydra's toolbar", () => {
  assert.match(tauri, /"hiddenTitle": true/);
  assert.match(tauri, /"titleBarStyle": "Overlay"/);
  assert.match(tauri, /"trafficLightPosition": \{ "x": 15, "y": 20 \}/);
  assert.match(desktop, /TitleBarStyle::Overlay/);
  assert.match(desktop, /\.hidden_title\(true\)/);
  assert.match(index, /<header class="topbar">[\s\S]*?<\/label>\s*<div class="topbar-drag-region" aria-hidden="true" data-tauri-drag-region><\/div>\s*<div class="top-actions">/);
  assert.doesNotMatch(index, /<header class="topbar" data-tauri-drag-region>/);
  assert.match(app, /classList\.toggle\("platform-macos", isMacOS\)/);
  assert.match(app, /"data-tauri-drag-region": true/);
  assert.match(styles, /\.platform-macos \.app-shell\s*\{[^}]*grid-template-rows:\s*56px 1fr/);
  assert.match(styles, /\.platform-macos \.topbar\s*\{[^}]*grid-template-columns:\s*minmax\(280px, 610px\) minmax\(0, 1fr\) auto;[^}]*padding:\s*7px 18px 7px 78px/);
  assert.match(styles, /\.topbar-drag-region\s*\{[^}]*align-self:\s*stretch;[^}]*min-width:\s*0/);
  assert.match(styles, /\.platform-macos #settings-button\s*\{\s*display:\s*none;/);
});

test("dark text and controls remain legible for every accent", () => {
  const accents = ["#5687bb", "#6574a8", "#826fa3", "#a56f5d", "#6f846f"];
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
  assert.match(index, /background:\s*#dce7f2/);
  assert.match(index, /id="app"[\s\S]*hidden/);
  assert.match(app, /function finishBoot\(\)/);
  assert.match(app, /splash\?\.remove\(\)/);
  assert.doesNotMatch(styles, /text-transform:\s*uppercase/);
  assert.doesNotMatch(styles, /button[^{]*:[^{]*\{[^}]*transform:/);
});

test("appearance and chamber tabs honor desktop input contracts", () => {
  assert.match(app, /function saveAppearanceChoice\(event\)/);
  assert.match(app, /runtime\("settings\.update", selected\)/);
  assert.match(app, /"stone-blue": "#5687bb"/);
  assert.match(app, /\["stone-blue", "Light blue"\]/);
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

test("the quiet toolbar keeps messages gray until unread mail arrives", () => {
  const sidebar = index.match(/<aside class="sidebar"[\s\S]*?<\/aside>/)?.[0] ?? "";
  assert.doesNotMatch(sidebar, /Messages|Reddit Bridge|Settings/);
  assert.match(index, /<div class="top-actions">[\s\S]*id="messages-button"[\s\S]*id="settings-button"/);
  assert.doesNotMatch(index, /class="brand"|id="sync-button"|id="compose-button"/);
  assert.match(index, /class="toolbar-icon-glyph message-icon"[\s\S]*viewBox="0 0 24 24"/);
  assert.match(index, /class="toolbar-icon-glyph settings-icon"[\s\S]*viewBox="0 0 14 14"/);
  assert.match(index, /id="message-badge" class="message-count" hidden/);
  assert.match(app, /messageRequestCount \?\? 0/);
  assert.match(app, /`Messages, \$\{unreadCount\} unread`/);
  assert.match(app, /messagesButton\.classList\.toggle\("has-unread", unreadCount > 0\)/);
  assert.match(styles, /\.message-icon\s*\{[^}]*width:\s*26px;[^}]*height:\s*26px;[^}]*color:\s*var\(--muted\)/);
  assert.match(styles, /\.message-button\.has-unread \.message-icon\s*\{[^}]*color:\s*#ff4500/);
  assert.match(styles, /\.message-count\s*\{[^}]*background:\s*#ff4500/);
});

test("posting is community-scoped and synchronization is ambient", () => {
  assert.doesNotMatch(index, /id="sync-button"|id="compose-button"/);
  assert.match(app, /function audienceBar\(community\)/);
  assert.match(app, /actionButton\("New post", \(\) => showComposer\(community\), "primary-button community-new-post"\)/);
  assert.match(app, /const AUTOMATIC_SYNC_INTERVAL_MS = 120_000/);
  assert.match(app, /async function automaticSync\(force = false\)/);
  assert.match(app, /await runtime\("sync\.now"\)/);
  assert.match(app, /scheduleAutomaticSync\(\)/);
});

test("Saved is explicit private saved-for-later memory, not browsing history", () => {
  assert.match(index, /data-nav="revisited" title="Posts saved privately for this persona"/);
  assert.match(index, /class="nav-icon"[\s\S]*Saved/);
  assert.doesNotMatch(index, /↺|>Revisited|>Revisit</);
  assert.match(app, /session\.route === "revisited" \? "Saved"/);
  assert.match(app, /this is not browsing history/);
  assert.match(app, /Nothing saved yet/);
  assert.match(app, /text: "Save", onclick: \(\) => showRevisit/);
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
  assert.match(app, /Why is this here\?/);
  assert.match(app, /Block locally/);
});

test("votes toggle conventionally and secondary post context lives in the overflow menu", () => {
  assert.match(app, /function currentPersonaVote\(target\)/);
  assert.match(app, /function toggleVote\(target, value\)/);
  assert.match(app, /currentPersonaVote\(target\) === value \? "0" : value/);
  assert.match(app, /"aria-pressed": active/);
  assert.doesNotMatch(app, /Reset vote|Reaffirm \+|Vote views|Feed reason/);
  assert.match(app, /function postActionMenu\(post, lens, community\)/);
  assert.match(app, /class: "community-menu-trigger post-menu-trigger"[^\n]*text: "⋮"/);
  assert.match(app, /item\("Why is this here\?"/);
  assert.match(app, /item\("Vote details"/);
});

test("post listings use a minimal old-Reddit hierarchy", () => {
  assert.match(app, /function blockArrowIcon\(direction\)/);
  assert.match(app, /M12 3 4 11h5v8h6v-8h5z/);
  assert.match(styles, /--vote-up:\s*#ff4500/);
  assert.match(styles, /--vote-down:\s*#4f72d8/);
  assert.match(styles, /\.post-card\s*\{[^}]*border:\s*0;[^}]*background:\s*transparent/);
  assert.doesNotMatch(styles, /\.post-card:hover/);
  assert.match(styles, /\.post-title\s*\{[^}]*system-ui/);
  assert.doesNotMatch(styles, /\.post-title\s*\{[^}]*Georgia/);
  assert.match(app, /ageElement\(post\.createdAt \?\? post\.editedAt, "post-age"\)/);
  assert.match(app, /title: exactDateTime\(value\)/);
  assert.match(styles, /\.content-age\s*\{\s*cursor:\s*default;/);
  assert.match(styles, /\.post-age\s*\{[^}]*font-size:\s*12px/);
  assert.match(styles, /\.post-title\s*\{[^}]*font:\s*500 15px/);
  assert.match(styles, /\.post-body\s*\{[^}]*font-size:\s*13px/);
  assert.match(styles, /\.community-chip, \.state-chip\s*\{[^}]*font-size:\s*12px/);
  assert.doesNotMatch(styles, /font-size:\s*(?:9|10|11)px/);
  assert.match(app, /class: "post-hydrants"/);
  assert.match(app, /item\("Post details"/);
  assert.match(app, /onclick: \(\) => setRoute\("community", name\)/);
});

test("text and local image listing previews are independently configurable", () => {
  assert.match(app, /settings\.show_text_previews !== false/);
  assert.match(app, /settings\.show_image_previews !== false/);
  assert.match(app, /textOnly && session\.state\.settings\?\.show_text_previews !== false/);
  assert.match(app, /function postImagePreview\(post\)/);
  assert.match(app, /invoke\("read_media_preview", \{ sha256: media\.sha256 \}\)/);
  assert.match(desktop, /fn read_media_preview\(sha256: &str\)/);
  assert.match(desktop, /content hash/);
  assert.match(styles, /\.post-image-preview img\s*\{[^}]*object-fit:\s*contain/);
});

test("React uses one Signal-style favorites-first emoji picker", () => {
  assert.match(app, /function emojiReactButton\(object, className = "text-action"\)/);
  assert.match(app, /class: `\$\{className\} emoji-react-button`/);
  assert.match(app, /"aria-label": "React with an emoji"/);
  assert.match(app, /"aria-haspopup": "dialog"/);
  assert.match(app, /"aria-expanded": "false"/);
  assert.match(app, /M19 2v6/);
  assert.doesNotMatch(app, /actionButton\("React"|text: "React", onclick: \(event\) => showEmojiReaction/);
  assert.match(app, /function showEmojiReaction\(event, object\)/);
  assert.match(app, /session\.emojiPicker\?\.target === object\.anchor && session\.emojiPicker\.trigger === trigger[\s\S]*closeEmojiReactionCallout\(\);[\s\S]*return;/);
  assert.match(app, /trigger\?\.setAttribute\?\.\("aria-expanded", "true"\)/);
  assert.match(app, /picker\.trigger\?\.setAttribute\?\.\("aria-expanded", "false"\)/);
  assert.match(app, /class: "emoji-reaction-callout"/);
  assert.match(app, /DEFAULT_FAVORITE_REACTION_EMOJIS = \["❤️", "👍", "👎", "😆", "😮", "😢", "🤔"\]/);
  assert.match(app, /DEFAULT_COMPACT_REACTION_SLOT_COUNT = 7/);
  assert.match(app, /FAVORITE_REACTION_EMOJIS_STORAGE_KEY = "hydra\.favoriteReactionEmojis\.v1"/);
  assert.doesNotMatch(app, /LEGACY_FAVORITE_REACTION_EMOJIS|PREVIOUS_FAVORITE_REACTION_EMOJIS/);
  assert.match(app, /renderCompactEmojiPicker\(picker\)/);
  assert.match(app, /class: "emoji-picker-expand"/);
  assert.match(app, /function renderExpandedEmojiPicker\(picker\)/);
  assert.match(app, /emojiPickerSection\("Favorites"[\s\S]*emojiPickerSection\("Recently Used"/);
  assert.match(app, /class: "emoji-slot-controls"[\s\S]*Show fewer quick reactions[\s\S]*Show more quick reactions/);
  assert.match(app, /picker\.favorites\.slice\(0, picker\.slotCount\)/);
  assert.match(app, /placeholder: "Search emoji"/);
  assert.doesNotMatch(app, /emoji-callout-note|emoji-custom-row|settings gear/i);
  assert.match(app, /FAVORITE_REACTION_EMOJIS_STORAGE_KEY/);
  assert.match(app, /RECENT_REACTION_EMOJIS_STORAGE_KEY/);
  assert.match(app, /rememberRecentReactionEmoji\(value\)/);
  assert.match(app, /onpointerdown:[\s\S]*setPointerCapture[\s\S]*onpointerup:/);
  assert.match(app, /document\.elementFromPoint[\s\S]*changeFavoriteReactionEmoji\(picker, drag\.emoji, false\)/);
  assert.match(app, /closeEmojiReactionCallout\(true\)/);
  assert.match(styles, /\.emoji-choice-grid\s*\{[^}]*grid-template-columns:\s*repeat\(8/);
  assert.match(styles, /\.emoji-reaction-callout\s*\{[^}]*border-radius:\s*22px/);
  assert.match(styles, /\.emoji-react-button\s*\{[^}]*border-radius:\s*6px/);
  assert.match(styles, /\.emoji-react-icon\s*\{[^}]*width:\s*18px/);
  assert.match(styles, /\.emoji-reaction-callout::before\s*\{[^}]*transform:\s*rotate\(45deg\)/);
  assert.match(styles, /\.emoji-reaction-callout\.is-above::before/);
  assert.match(styles, /\.emoji-category-navigation button\s*\{[^}]*filter:\s*grayscale\(1\);[^}]*opacity:\s*\.68/);
  assert.match(styles, /\.emoji-slot-controls\s*\{[^}]*grid-template-columns:\s*22px auto 22px/);
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
  assert.match(app, /function undoIcon\(\)/);
  assert.match(app, /M9 14 4 9l5-5/);
});

test("community routes use one compact heading with art and actions", () => {
  assert.doesNotMatch(app, /function renderBrand\(\)/);
  assert.doesNotMatch(index, /class="brand"|brand-mark|brand-copy/);
  assert.match(app, /function communityViewHeader\(community, title, extras = \[\]\)/);
  assert.match(app, /function communityActionMenu\(community\)/);
  assert.match(app, /class: "community-header-actions"[^\n]*\.\.\.extras, communityActionMenu\(community\)/);
  assert.match(app, /text: "\/h\/"/);
  assert.match(app, /text: "\/r\/"/);
  assert.match(app, /class: "community-menu-trigger"[^\n]*text: "⋮"/);
  assert.match(app, /function renderCommunityNormBanner\(community\)/);
  assert.match(app, /if \(!norms\.length\) return null;/);
  assert.doesNotMatch(app, /class: "community-tools"/);
  assert.match(app, /topicIdenticon\(community\)/);
  assert.match(app, /class: "community-heading-image"/);
  assert.match(app, /communityAppearances/);
  assert.match(app, /showCommunityAppearanceEditor/);
  assert.match(app, /community_appearance\.set/);
  assert.match(app, /appearance_source\.set/);
  assert.match(app, /Follow their community images/);
  assert.match(app, /function showPersonaProfile/);
  assert.doesNotMatch(app, /function showAppearanceSources/);
  assert.doesNotMatch(app, /field\("SHA-256"/);
  assert.match(styles, /\.community-image-preview/);
  assert.match(styles, /\.view-header\s*\{[^}]*height:\s*82px;[^}]*padding:\s*5px 28px/);
  assert.match(styles, /\.community-heading-art\s*\{[^}]*max-width:\s*100px;[^}]*height:\s*72px/);
  assert.match(styles, /\.community-heading-image\s*\{[^}]*max-width:\s*100px;[^}]*max-height:\s*100%;[^}]*width:\s*auto;[^}]*height:\s*72px/);
  assert.match(styles, /\.community-menu-trigger/);
  assert.match(styles, /\.community-menu-popover/);
  assert.match(styles, /\.community-norm-banner/);
  assert.match(desktop, /inspect_community_image/);
  assert.match(desktop, /MAX_COMMUNITY_IMAGE_BYTES/);
  assert.match(desktop, /community_image_mime/);
  assert.match(app, /invoke\("inspect_community_image", \{ url \}\)/);
  assert.match(app, /That image did not respond within 15 seconds|Checking the image/);
  assert.match(app, /status\.classList\.add\("is-error"\)/);
  assert.doesNotMatch(app, /fetch\(appearance\.url/);
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
