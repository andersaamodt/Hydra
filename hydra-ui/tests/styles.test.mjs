import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const styles = await readFile(new URL("../styles.css", import.meta.url), "utf8");
const app = await readFile(new URL("../app.js", import.meta.url), "utf8");

test("light mode supplies semantic surfaces instead of inheriting dark literals", () => {
  for (const variable of [
    "--nav-ink",
    "--bar",
    "--card",
    "--card-hover",
    "--card-soft",
    "--card-subtle",
    "--body-ink",
    "--modal",
  ]) {
    assert.match(styles, new RegExp(`:root\\[data-theme=\"light\"\\][\\s\\S]*${variable}:`));
    assert.match(styles, new RegExp(`:root\\[data-theme=\"system\"\\][\\s\\S]*${variable}:`));
  }

  assert.match(styles, /\.post-card\s*\{[^}]*background:\s*var\(--card\)/);
  assert.match(styles, /\.context-card\s*\{[^}]*background:\s*var\(--card-soft\)/);
  assert.match(styles, /\.lens-bar\s*\{[^}]*background:\s*var\(--bar\)/);
  assert.match(styles, /\.modal\s*\{[^}]*background:\s*var\(--modal\)/);
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
