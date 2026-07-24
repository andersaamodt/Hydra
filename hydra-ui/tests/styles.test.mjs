import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const styles = await readFile(new URL("../styles.css", import.meta.url), "utf8");
const app = await readFile(new URL("../app.js", import.meta.url), "utf8");
const index = await readFile(new URL("../index.html", import.meta.url), "utf8");

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
  assert.doesNotMatch(index, /<small>I’m with the banned\.<\/small>/);
  assert.doesNotMatch(app, /Your living memory|No rulers, only lenses|view-kicker|view-subtitle|empty-mark/);
  assert.match(app, /function viewHeader\(title, extras = \[\]\)/);
  assert.doesNotMatch(styles, /radial-gradient|linear-gradient|backdrop-filter|border-radius:\s*999px/);
  assert.match(app, /function settingsGroup\(title, children, open = false\)/);
  assert.match(app, /settingsGroup\("Nostr and media"/);
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

test("the Reddit Bridge exposes imported posts and comments with exact source links", () => {
  assert.match(app, /Imported Reddit writing/);
  assert.match(app, /item\.externalSource/);
  assert.match(app, /Copy source link/);
  assert.match(app, /slice\(0,\s*25\)/);
  assert.match(styles, /\.source-link\s*\{[^}]*overflow-wrap:\s*anywhere/);
});

test("startup uses the real icon and one atomic splash handoff", () => {
  assert.match(index, /id="boot-splash"[\s\S]*src="hydra-icon\.svg"/);
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

test("background Reddit refresh cannot overwrite a newer interaction", () => {
  assert.match(app, /epoch !== session\.reddit\.requestEpoch/);
  assert.match(app, /session\.busy \|\| modalRoot\.childElementCount \|\| document\.hidden/);
  assert.match(app, /document\.addEventListener\("visibilitychange"/);
});
