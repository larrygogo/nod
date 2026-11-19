// 16 px line icons (1.75 stroke, currentColor), drawn for Nod in the Lucide style.
import type { JSX } from "preact";

const paths: Record<string, JSX.Element> = {
  home: <><path d="M3 10.5 12 3l9 7.5" /><path d="M5 9.5V20h5v-6h4v6h5V9.5" /></>,
  list: <><path d="M8 6h13M8 12h13M8 18h13" /><circle cx="3.5" cy="6" r="1" /><circle cx="3.5" cy="12" r="1" /><circle cx="3.5" cy="18" r="1" /></>,
  puzzle: <path d="M10 3h4v2.5a1.5 1.5 0 0 0 3 0V3h4v7h-2.5a1.5 1.5 0 0 0 0 3H21v8h-7v-2.5a1.5 1.5 0 0 0-3 0V21H3v-8h2.5a1.5 1.5 0 0 0 0-3H3V3h7z" />,
  settings: <><circle cx="12" cy="12" r="3" /><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z" /></>,
  play: <path d="M7 4v16l13-8z" />,
  pause: <><path d="M8 5v14M16 5v14" /></>,
  refresh: <><path d="M20 11a8 8 0 0 0-14.9-3M4 13a8 8 0 0 0 14.9 3" /><path d="M4 4v4h4M20 20v-4h-4" /></>,
  external: <><path d="M14 4h6v6M20 4l-9 9" /><path d="M18 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h5" /></>,
  search: <><circle cx="11" cy="11" r="7" /><path d="m20 20-3.5-3.5" /></>,
  check: <path d="m5 12.5 4.5 4.5L19 7.5" />,
  x: <path d="M6 6l12 12M18 6 6 18" />,
  alert: <><path d="M12 3 2 20h20z" /><path d="M12 10v4M12 17.5v.01" /></>,
  info: <><circle cx="12" cy="12" r="9" /><path d="M12 11v5M12 7.5v.01" /></>,
  chevron: <path d="m9 6 6 6-6 6" />,
  plus: <path d="M12 5v14M5 12h14" />,
  trash: <><path d="M4 7h16M10 11v6M14 11v6" /><path d="M6 7l1 13h10l1-13M9 7V4h6v3" /></>,
  folder: <path d="M3 6a1 1 0 0 1 1-1h5l2 2h9a1 1 0 0 1 1 1v10a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1z" />,
  bell: <><path d="M6 16V11a6 6 0 0 1 12 0v5l1.5 2h-15z" /><path d="M10 20.5a2 2 0 0 0 4 0" /></>,
  hand: <><path d="M8 13V5.5a1.5 1.5 0 0 1 3 0V11" /><path d="M11 10V4a1.5 1.5 0 0 1 3 0v6" /><path d="M14 10.5V6a1.5 1.5 0 0 1 3 0v8a6 6 0 0 1-6 6h-.5a6 6 0 0 1-4.6-2.2L3.6 15a1.6 1.6 0 0 1 2.4-2l2 1.5" /></>,
  shield: <><path d="M12 3 4.5 6v5.5c0 4.5 3.2 8.2 7.5 9.5 4.3-1.3 7.5-5 7.5-9.5V6z" /><path d="m9 12 2.2 2.2L15.5 10" /></>,
  file: <><path d="M14 3H6a1 1 0 0 0-1 1v16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1V8z" /><path d="M14 3v5h5" /></>,
  globe: <><circle cx="12" cy="12" r="9" /><path d="M3 12h18M12 3a14 14 0 0 1 0 18M12 3a14 14 0 0 0 0 18" /></>,
  rules: <><path d="M4 6h10M4 12h7M4 18h10" /><path d="m16 9 3 3-3 3" /></>,
  key: <><circle cx="8" cy="15" r="4" /><path d="m11 12 9-9M17 6l3 3M15 8l2 2" /></>,
};

export function Icon({ name, size = 16 }: { name: keyof typeof paths | string; size?: number }) {
  return (
    <svg class="icon" width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor"
      stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      {paths[name] ?? paths.info}
    </svg>
  );
}

/** Product tiles: the initial in the product's colour (no third-party logos are bundled). */
const brands: Record<string, [string, string]> = {
  github: ["#24292f", "GH"], gitee: ["#c71d23", "码"], gitea: ["#609926", "Gt"],
  claude: ["#d97757", "C"], openai: ["#10a37f", "Cx"], gemini: ["#4f7df3", "G"], api: ["#6366f1", "API"],
  bell: ["#64748b", ""], feishu: ["#3370ff", "飞"], dingtalk: ["#1a7cff", "钉"], wecom: ["#2a8af7", "企"],
  slack: ["#611f69", "S"], webhook: ["#475569", "{ }"], plugin: ["#71717a", "+"], rules: ["#52525b", ""],
};

/** Brands whose colour is near black or grey: a tinted tile would be muddy, so they use the neutral surface. */
const neutral = new Set(["github", "bell", "webhook", "plugin", "rules"]);

export function Tile({ icon, size = 32 }: { icon: string; size?: number }) {
  const [c, txt] = brands[icon] ?? brands.plugin;
  // Below 24px a pale tile with same-hue ink drops under ~8px text and reads as a broken placeholder:
  // small tiles switch to a solid fill with white ink and a larger letter, like a favicon.
  const small = size < 24;
  const ratio = small ? [0.62, 0.48, 0.36][Math.min(txt.length, 3) - 1] ?? 0.36 : txt.length > 2 ? 0.3 : 0.4;
  const style = { "--c": c, width: size, height: size, fontSize: size * ratio, borderRadius: Math.round(size * (small ? 0.28 : 0.26)) } as unknown as JSX.CSSProperties;
  return (
    <span class={`tile${neutral.has(icon) || !brands[icon] ? " neutral" : ""}${small ? " sm" : ""}`} style={style}>
      {icon === "bell" || icon === "rules" ? <Icon name={icon} size={size * 0.55} /> : txt}
    </span>
  );
}
