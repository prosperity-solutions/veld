/**
 * The banner that says veld's own state is in trouble.
 *
 * Deliberately thin: every rule about *whether* to show it, and what it says,
 * lives in `model.ts` where the tests can reach it. This file is layout.
 *
 * It sits in the window's top column next to the offline and channel-down
 * notices, but it is a Mantine `Alert` rather than one of their thin strips —
 * this one carries a title, a sentence and an action, and it is the only one of
 * the three that a person has to *do* something about.
 */

import { Alert, Button, Group, Text } from "@mantine/core";
import { IconDatabaseExclamation } from "@tabler/icons-react";
import type { Notice } from "./model";

export function DbHealthBanner(props: {
  notice: Notice;
  /** Opens the confirmation dialog. Absent when there is nothing to restore. */
  onRestore?: () => void;
  onDetails: () => void;
}) {
  const { notice } = props;
  if (notice.severity === "none") return null;

  return (
    <Alert
      variant="light"
      color={notice.severity === "error" ? "red" : "yellow"}
      icon={<IconDatabaseExclamation size={18} />}
      title={notice.headline}
      radius={0}
      style={{ flex: "none" }}
      // Not dismissible, and that is the point. The condition is continuous and
      // the cost of missing it is the incident this feature was filed against —
      // a fault that went unnoticed for 17 hours. It disappears when it is
      // fixed, not when it is acknowledged.
    >
      <Group gap="sm" wrap="wrap" align="center">
        <Text size="sm" style={{ flex: 1, minWidth: 240 }}>
          {notice.detail}
        </Text>
        {notice.canRestore && props.onRestore && (
          <Button
            size="xs"
            color={notice.severity === "error" ? "red" : "yellow"}
            onClick={props.onRestore}
          >
            Restore newest backup…
          </Button>
        )}
        <Button size="xs" variant="default" onClick={props.onDetails}>
          Details
        </Button>
      </Group>
    </Alert>
  );
}
