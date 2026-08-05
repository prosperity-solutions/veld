/**
 * The icons a config-declared pane may name.
 *
 * A static map, not a dynamic import, and that is why the config takes an
 * allowlist rather than any Tabler name: the bundle has to contain every icon
 * that can be rendered, and `import(\`@tabler/icons-react/\${name}\`)` would
 * either pull in the whole set (thousands of components) or resolve to nothing
 * at runtime.
 *
 * The keys are exactly `veld_core::ide::PANE_ICON_NAMES`, which is in turn
 * gated against the JSON schema's `$defs.paneIconName` enum by a Rust test —
 * and `paneIconNamesMatchTheSchema` in `paneIcons.test.ts` closes the loop on
 * this side, so a name can never be accepted by the config and then render
 * blank here.
 */
import {
  IconAtom,
  IconBolt,
  IconBook,
  IconBrain,
  IconBug,
  IconBulb,
  IconChartLine,
  IconCloud,
  IconCode,
  IconCompass,
  IconCpu,
  IconDatabase,
  IconFlask,
  IconGitBranch,
  IconKey,
  IconMap,
  IconMessageChatbot,
  IconNotebook,
  IconPackage,
  IconPlayerPlay,
  IconPlug,
  IconPuzzle,
  IconRefresh,
  IconRobot,
  IconRocket,
  IconSearch,
  IconServer,
  IconShield,
  IconSparkles,
  IconTerminal2,
  IconTool,
  IconWand,
} from "@tabler/icons-react";
import type { PaneIcon } from "../api";

// `ComponentType`, not a function type: Tabler icons are `forwardRef`
// objects, which a bare function signature only accepts by structural luck.
type IconComponent = React.ComponentType<{ size?: number }>;

export const PANE_ICONS: Record<string, IconComponent> = {
  atom: IconAtom,
  bolt: IconBolt,
  book: IconBook,
  brain: IconBrain,
  bug: IconBug,
  bulb: IconBulb,
  "chart-line": IconChartLine,
  cloud: IconCloud,
  code: IconCode,
  compass: IconCompass,
  cpu: IconCpu,
  database: IconDatabase,
  flask: IconFlask,
  "git-branch": IconGitBranch,
  key: IconKey,
  map: IconMap,
  "message-chatbot": IconMessageChatbot,
  notebook: IconNotebook,
  package: IconPackage,
  "player-play": IconPlayerPlay,
  plug: IconPlug,
  puzzle: IconPuzzle,
  refresh: IconRefresh,
  robot: IconRobot,
  rocket: IconRocket,
  search: IconSearch,
  server: IconServer,
  shield: IconShield,
  sparkles: IconSparkles,
  "terminal-2": IconTerminal2,
  tool: IconTool,
  wand: IconWand,
};

/**
 * Render a pane's icon at `size`, falling back to a terminal glyph.
 *
 * The fallback matters more than it looks: a pane whose icon name this build
 * does not know still has to render *something* tab-sized, or the tab strip
 * reflows around a hole.
 */
export function paneIcon(icon: PaneIcon | undefined, size: number): React.ReactNode {
  if (icon?.kind === "emoji") {
    // Rendered as text at a size that matches the line-drawn glyphs beside it;
    // an emoji at the icon's nominal size reads noticeably larger.
    return (
      <span className="pane-emoji" style={{ fontSize: size }} aria-hidden>
        {icon.value}
      </span>
    );
  }
  const Icon = (icon && PANE_ICONS[icon.value]) || IconTerminal2;
  return <Icon size={size} />;
}
