/**
 * The first-run screen: what Veld is, and the one thing to do about it.
 *
 * **Stateless, and it must stay that way.** This is shown because the user has
 * zero projects, full stop — no seen-flag, no dismiss, nothing persisted. It
 * un-shows itself the moment a project is imported and comes back if the last
 * one is ever removed. A dismissable first-run screen is how you strand somebody
 * on the blank page this surface exists to replace, and the fact that it would
 * only strand them *once* is not a defence.
 */

import { Button } from "@mantine/core";
import { IconFolderPlus } from "@tabler/icons-react";

import { Wordmark } from "./Brand";
import { IDENTITY } from "./content";
import { PromoSection } from "./Section";

export function StartScreen(props: { onImport: () => void }) {
  return (
    <div className="start-screen">
      <div className="start-screen-inner">
        <header className="start-screen-head">
          <Wordmark height={38} />
          <p className="start-screen-tagline">
            An IDE for working with coding agents — deliberately without a code editor.
          </p>
        </header>

        <div className="promo-grid">
          {IDENTITY.map((s) => (
            <PromoSection key={s.id} section={s} />
          ))}
        </div>

        <footer className="start-screen-cta">
          <Button size="md" leftSection={<IconFolderPlus size={16} />} onClick={props.onImport}>
            Import your first project
          </Button>
          <p className="start-screen-hint">
            Point Veld at any git repository. Importing only reads it — the repo and its
            worktrees show up here straight away.
          </p>
        </footer>
      </div>
    </div>
  );
}
