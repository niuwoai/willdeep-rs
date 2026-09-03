// 一级入口栏。对话固定在最上，插件目的地排在它下面，插件中心殿后。
//
// 与 macOS 宿主同一条规则：最多固定 5 个插件目的地，多出来的收进「更多插件」。
// 选中状态只用灰度表达，插件的品牌色不参与——一栏花花绿绿的入口会让用户
// 分不清哪个是当前位置。

import { Box, Flex, Text } from "@chakra-ui/react";
import { useState } from "react";
import type { Messages } from "./i18n";
import { SfIcon } from "./sfSymbols";
import { MAX_PINNED_DESTINATIONS, type PluginDestinationView, type PluginView } from "./plugins";

export type RailSelection = { kind: "conversation" } | { kind: "center" } | { kind: "plugin"; qualifiedId: string };

type Props = {
  entries: Array<{ plugin: PluginView; destination: PluginDestinationView }>;
  selection: RailSelection;
  messages: Messages;
  onSelect: (selection: RailSelection) => void;
};

export function PluginRail({ entries, selection, messages, onSelect }: Props) {
  const [showOverflow, setShowOverflow] = useState(false);
  const pinned = entries.slice(0, MAX_PINNED_DESTINATIONS);
  const overflow = entries.slice(MAX_PINNED_DESTINATIONS);
  const activeId = selection.kind === "plugin" ? selection.qualifiedId : null;

  const button = (
    key: string,
    label: string,
    icon: string | null | undefined,
    active: boolean,
    onClick: () => void
  ) => (
    <button
      key={key}
      type="button"
      className={`rail-button${active ? " active" : ""}`}
      title={label}
      aria-label={label}
      aria-current={active ? "page" : undefined}
      onClick={onClick}
    >
      <SfIcon name={icon} size={20} />
      <Text className="rail-label">{label}</Text>
    </button>
  );

  return (
    <Flex as="nav" className="plugin-rail" direction="column" aria-label={messages.pluginNavigation}>
      {button(
        "conversation",
        messages.conversation,
        "sf:text.badge.plus",
        selection.kind === "conversation",
        () => onSelect({ kind: "conversation" })
      )}
      <Box className="rail-divider" />
      {pinned.map(({ destination }) =>
        button(
          destination.qualified_id,
          destination.title,
          destination.icon,
          activeId === destination.qualified_id,
          () => onSelect({ kind: "plugin", qualifiedId: destination.qualified_id })
        )
      )}
      {overflow.length > 0 && (
        <Box className="rail-overflow">
          {button("more", messages.pluginMore, "sf:square.grid.2x2", false, () =>
            setShowOverflow((current) => !current)
          )}
          {showOverflow && (
            <Box className="rail-overflow-menu">
              {overflow.map(({ destination }) => (
                <button
                  key={destination.qualified_id}
                  type="button"
                  className={`rail-overflow-item${activeId === destination.qualified_id ? " active" : ""}`}
                  onClick={() => {
                    setShowOverflow(false);
                    onSelect({ kind: "plugin", qualifiedId: destination.qualified_id });
                  }}
                >
                  <SfIcon name={destination.icon} size={16} />
                  {destination.title}
                </button>
              ))}
            </Box>
          )}
        </Box>
      )}
      <Box flex="1" />
      {button("center", messages.pluginCenter, "sf:gearshape", selection.kind === "center", () =>
        onSelect({ kind: "center" })
      )}
    </Flex>
  );
}
