import { Fragment } from "react";
import { Badge, Group, Kbd, ScrollArea, Stack, Table, Text } from "@mantine/core";
import { Modal } from "../components/dialogs";
import { CATEGORY_ORDER, SHORTCUTS, categoryLabel, comboTokens, isMac } from "./registry";

/**
 * The Shortcuts overview: every keyboard shortcut Veld binds — in the IDE's
 * own keydown effect, an Electron menu accelerator, or a terminal's key
 * substitution — grouped and shown with the platform's own modifier glyphs.
 * Read straight from `./registry`, the single source of truth for this list;
 * see its doc comment for what "single source" does and doesn't cover.
 */
export function ShortcutsDialog(props: { onClose: () => void }) {
  const mac = isMac();
  return (
    <Modal title="Keyboard shortcuts" onClose={props.onClose} size={860}>
      <ScrollArea.Autosize mah="min(70vh, 560px)" type="auto" offsetScrollbars>
        <Stack gap="lg">
          {CATEGORY_ORDER.map((category) => {
            const rows = SHORTCUTS.filter((s) => s.category === category);
            if (rows.length === 0) return null;
            return (
              <div key={category}>
                <Text size="xs" fw={600} tt="uppercase" c="dimmed" mb={6}>
                  {categoryLabel(category)}
                </Text>
                <Table verticalSpacing="xs" withRowBorders>
                  <Table.Tbody>
                    {rows.map((s) => (
                      <Table.Tr key={s.id}>
                        <Table.Td style={{ whiteSpace: "nowrap", width: "26%" }}>
                          {s.title}
                        </Table.Td>
                        <Table.Td style={{ whiteSpace: "nowrap", width: "24%" }}>
                          <Group gap={6} wrap="nowrap">
                            {s.combos.map((combo, i) => (
                              <Fragment key={i}>
                                {i > 0 && (
                                  <Text size="xs" c="dimmed">
                                    /
                                  </Text>
                                )}
                                <Group gap={2} wrap="nowrap">
                                  {comboTokens(combo, mac).map((token, j) => (
                                    <Kbd key={j} size="sm">
                                      {token}
                                    </Kbd>
                                  ))}
                                </Group>
                              </Fragment>
                            ))}
                          </Group>
                        </Table.Td>
                        <Table.Td>
                          <Text size="sm" c="dimmed">
                            {s.description}
                          </Text>
                        </Table.Td>
                        {/* Its own column, `width: 1` shrink-to-content —
                            inline with the description (the previous layout)
                            let a long description's own `nowrap` Group push
                            the badge past the row's width with nothing left
                            to render it in, which is what truncated it. */}
                        <Table.Td style={{ whiteSpace: "nowrap", width: 1 }}>
                          {s.desktopOnly && (
                            <Badge size="xs" variant="light" color="gray">
                              Desktop app
                            </Badge>
                          )}
                        </Table.Td>
                      </Table.Tr>
                    ))}
                  </Table.Tbody>
                </Table>
              </div>
            );
          })}
        </Stack>
      </ScrollArea.Autosize>
    </Modal>
  );
}
