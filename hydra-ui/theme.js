export const ACCENT_COLORS = {
  "stone-blue": "#5687bb",
  indigo: "#6574a8",
  violet: "#826fa3",
  terracotta: "#a56f5d",
  moss: "#6f846f",
};

const HEX_COLOR = /^#[0-9a-f]{6}$/;

export function canonicalColor(value, fallback) {
  const color = String(value ?? "").trim().toLowerCase();
  return HEX_COLOR.test(color) ? color : fallback;
}

function rgb(color) {
  return [1, 3, 5].map((index) => Number.parseInt(color.slice(index, index + 2), 16));
}

function hex(channels) {
  return `#${channels.map((channel) => Math.round(channel).toString(16).padStart(2, "0")).join("")}`;
}

function mix(foreground, amount, background) {
  const front = rgb(foreground);
  const back = rgb(background);
  return hex(front.map((channel, index) => channel * amount + back[index] * (1 - amount)));
}

function luminance(color) {
  const channels = rgb(color)
    .map((channel) => channel / 255)
    .map((channel) => channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4);
  return 0.2126 * channels[0] + 0.7152 * channels[1] + 0.0722 * channels[2];
}

export function contrast(first, second) {
  const [light, dark] = [luminance(first), luminance(second)].sort((left, right) => right - left);
  return (light + 0.05) / (dark + 0.05);
}

// Keeps the chosen hue while ensuring Hydra's derived primary control has legible text.
export function accessibleAccent(color, mode) {
  const input = canonicalColor(color, ACCENT_COLORS["stone-blue"]);
  const dark = mode === "dark";
  const target = dark ? "#ffffff" : "#000000";
  const anchor = dark ? "#e5edf5" : "#1b2d40";
  const onAccent = dark ? "#18212b" : "#ffffff";
  const amount = dark ? 0.58 : 0.64;
  for (let step = 0; step <= 100; step += 1) {
    const candidate = mix(input, 1 - step / 100, target);
    const primaryControl = mix(candidate, amount, anchor);
    if (contrast(primaryControl, onAccent) >= 4.5 && contrast(candidate, onAccent) >= 4.5) return candidate;
  }
  return target;
}

export function resolvedThemeColors(settings = {}, communityScheme = null, mode = "light") {
  const defaultAccent = ACCENT_COLORS[settings.accent] ?? ACCENT_COLORS["stone-blue"];
  const useCommunity = settings.use_community_colors !== false && communityScheme;
  const baseValue = useCommunity
    ? communityScheme[mode === "dark" ? "darkBase" : "lightBase"]
    : defaultAccent;
  const accentValue = useCommunity
    ? communityScheme[mode === "dark" ? "darkAccent" : "lightAccent"]
    : defaultAccent;
  const base = canonicalColor(baseValue, defaultAccent);
  return { base, accent: accessibleAccent(accentValue, mode) };
}

export function suggestedDarkColors(lightBase, lightAccent) {
  return {
    darkBase: mix(canonicalColor(lightBase, ACCENT_COLORS["stone-blue"]), 0.22, "#101419"),
    darkAccent: accessibleAccent(lightAccent, "dark"),
  };
}
