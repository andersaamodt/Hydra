import test from "node:test";
import assert from "node:assert/strict";

import { accessibleAccent, contrast, resolvedThemeColors } from "../theme.js";

test("community schemes select independent light and dark seeds", () => {
  const scheme = {
    lightBase: "#b9d3eb",
    lightAccent: "#326a9d",
    darkBase: "#182634",
    darkAccent: "#82b9e7",
  };
  assert.deepEqual(resolvedThemeColors({}, scheme, "light"), {
    base: "#b9d3eb",
    accent: accessibleAccent("#326a9d", "light"),
  });
  assert.deepEqual(resolvedThemeColors({}, scheme, "dark"), {
    base: "#182634",
    accent: accessibleAccent("#82b9e7", "dark"),
  });
});

test("community colors can be disabled without changing the user's mode", () => {
  const scheme = { lightBase: "#ffffff", lightAccent: "#ffffff" };
  assert.deepEqual(
    resolvedThemeColors({ accent: "moss", use_community_colors: false }, scheme, "light"),
    { base: "#6f846f", accent: accessibleAccent("#6f846f", "light") },
  );
});

test("authored accents are normalized until primary-control text is accessible", () => {
  const light = accessibleAccent("#ffffff", "light");
  const dark = accessibleAccent("#000000", "dark");
  assert.ok(contrast(light, "#ffffff") >= 4.5);
  assert.ok(contrast(dark, "#18212b") >= 4.5);
});
