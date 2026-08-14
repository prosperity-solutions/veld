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
  IconAlertTriangle,
  IconAppWindow,
  IconAtom,
  IconBan,
  IconBolt,
  IconBook,
  IconBrain,
  IconBrandDocker,
  IconBrandGithub,
  IconBrandGitlab,
  IconBrandSlack,
  IconBrandVscode,
  IconBrowser,
  IconBug,
  IconBulb,
  IconChartLine,
  IconCheck,
  IconCircleCheck,
  IconCircleDashed,
  IconCircleX,
  IconClock,
  IconCloud,
  IconCloudUpload,
  IconCode,
  IconCoin,
  IconCompass,
  IconCpu,
  IconDatabase,
  IconDeviceDesktop,
  IconDownload,
  IconExternalLink,
  IconEye,
  IconFileCode,
  IconFlag,
  IconFlask,
  IconFolder,
  IconGauge,
  IconGitBranch,
  IconGitCommit,
  IconGitMerge,
  IconGitPullRequest,
  IconHelpCircle,
  IconHistory,
  IconHourglass,
  IconInfoCircle,
  IconKey,
  IconLink,
  IconListCheck,
  IconLock,
  IconLockOpen,
  IconMail,
  IconMap,
  IconMessageChatbot,
  IconNotebook,
  IconPackage,
  IconPalette,
  IconPlayerPause,
  IconPlayerPlay,
  IconPlug,
  IconPuzzle,
  IconRefresh,
  IconRobot,
  IconRocket,
  IconSearch,
  IconServer,
  IconShield,
  IconShieldCheck,
  IconSparkles,
  IconStar,
  IconTag,
  IconTerminal,
  IconTerminal2,
  IconTool,
  IconTrendingDown,
  IconTrendingUp,
  IconUpload,
  IconUser,
  IconUsers,
  IconWand,
  IconX,
} from "@tabler/icons-react";
import type { PaneIcon } from "../api";

// `ComponentType`, not a function type: Tabler icons are `forwardRef`
// objects, which a bare function signature only accepts by structural luck.
type IconComponent = React.ComponentType<{ size?: number }>;

export const PANE_ICONS: Record<string, IconComponent> = {
  "alert-triangle": IconAlertTriangle,
  "app-window": IconAppWindow,
  "atom": IconAtom,
  "ban": IconBan,
  "bolt": IconBolt,
  "book": IconBook,
  "brain": IconBrain,
  "brand-docker": IconBrandDocker,
  "brand-github": IconBrandGithub,
  "brand-gitlab": IconBrandGitlab,
  "brand-slack": IconBrandSlack,
  "brand-vscode": IconBrandVscode,
  "browser": IconBrowser,
  "bug": IconBug,
  "bulb": IconBulb,
  "chart-line": IconChartLine,
  "check": IconCheck,
  "circle-check": IconCircleCheck,
  "circle-dashed": IconCircleDashed,
  "circle-x": IconCircleX,
  "clock": IconClock,
  "cloud": IconCloud,
  "cloud-upload": IconCloudUpload,
  "code": IconCode,
  "coin": IconCoin,
  "compass": IconCompass,
  "cpu": IconCpu,
  "database": IconDatabase,
  "device-desktop": IconDeviceDesktop,
  "download": IconDownload,
  "external-link": IconExternalLink,
  "eye": IconEye,
  "file-code": IconFileCode,
  "flag": IconFlag,
  "flask": IconFlask,
  "folder": IconFolder,
  "gauge": IconGauge,
  "git-branch": IconGitBranch,
  "git-commit": IconGitCommit,
  "git-merge": IconGitMerge,
  "git-pull-request": IconGitPullRequest,
  "help-circle": IconHelpCircle,
  "history": IconHistory,
  "hourglass": IconHourglass,
  "info-circle": IconInfoCircle,
  "key": IconKey,
  "link": IconLink,
  "list-check": IconListCheck,
  "lock": IconLock,
  "lock-open": IconLockOpen,
  "mail": IconMail,
  "map": IconMap,
  "message-chatbot": IconMessageChatbot,
  "notebook": IconNotebook,
  "package": IconPackage,
  "palette": IconPalette,
  "player-pause": IconPlayerPause,
  "player-play": IconPlayerPlay,
  "plug": IconPlug,
  "puzzle": IconPuzzle,
  "refresh": IconRefresh,
  "robot": IconRobot,
  "rocket": IconRocket,
  "search": IconSearch,
  "server": IconServer,
  "shield": IconShield,
  "shield-check": IconShieldCheck,
  "sparkles": IconSparkles,
  "star": IconStar,
  "tag": IconTag,
  "terminal": IconTerminal,
  "terminal-2": IconTerminal2,
  "tool": IconTool,
  "trending-down": IconTrendingDown,
  "trending-up": IconTrendingUp,
  "upload": IconUpload,
  "user": IconUser,
  "users": IconUsers,
  "wand": IconWand,
  "x": IconX,
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
