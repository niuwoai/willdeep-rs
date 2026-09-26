// 一级入口栏。对话固定在最上，插件目的地排在它下面，插件中心殿后。
//
// 与 macOS 宿主同一条规则：最多固定 5 个插件目的地，多出来的收进「更多插件」。
// 选中状态只用灰度表达，插件的品牌色不参与——一栏入口花花绿绿会让用户
// 分不清哪个是当前位置。
//
// macOS 把插件放在侧栏「插件」分组里，一行图标 + 名称；Web 宿主保留竖向入口栏
// 的布局（平台差异），但分组标题、图标取法和灰度层次与那边一致。

import { Box, Flex, Text } from "@chakra-ui/react";
import { useState, type ReactNode } from "react";
import type { Messages } from "./i18n";
import { PluginIcon, SfIcon } from "./sfSymbols";
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
  const overflowActive = overflow.some(({ destination }) => destination.qualified_id === activeId);

  const button = (key: string, label: string, icon: ReactNode, active: boolean, onClick: () => void) => (
    <button
      key={key}
      type="button"
      className={`rail-button${active ? " active" : ""}`}
      title={label}
      aria-label={label}
      aria-current={active ? "page" : undefined}
      onClick={onClick}
    >
      {icon}
      <Text className="rail-label">{label}</Text>
    </button>
  );

  return (
    <Flex as="nav" className="plugin-rail" direction="column" aria-label={messages.pluginNavigation}>
      {button(
        "conversation",
        messages.conversation,
        <SfIcon name="sf:text.badge.plus" size={20} />,
        selection.kind === "conversation",
        () => onSelect({ kind: "conversation" })
      )}
      <Box className="rail-divider" />
      {entries.length > 0 && <Text className="rail-section-label">{messages.pluginSectionTitle}</Text>}
      {pinned.map(({ plugin, destination }) =>
        button(
          destination.qualified_id,
          destination.title,
          <PluginIcon pluginId={plugin.id} icon={destination.icon} size={20} />,
          activeId === destination.qualified_id,
          () => onSelect({ kind: "plugin", qualifiedId: destination.qualified_id })
        )
      )}
      {overflow.length > 0 && (
        <Box className="rail-overflow">
          {button(
            "more",
            messages.pluginMore,
            <SfIcon name="sf:ellipsis.circle" size={20} />,
            overflowActive,
            () => setShowOverflow((current) => !current)
          )}
          {showOverflow && (
            <Box className="rail-overflow-menu" role="menu">
              <Text className="rail-overflow-heading">{messages.pluginMore}</Text>
              {overflow.map(({ plugin, destination }) => (
                <button
                  key={destination.qualified_id}
                  type="button"
                  role="menuitem"
                  className={`rail-overflow-item${activeId === destination.qualified_id ? " active" : ""}`}
                  aria-current={activeId === destination.qualified_id ? "page" : undefined}
                  onClick={() => {
                    setShowOverflow(false);
                    onSelect({ kind: "plugin", qualifiedId: destination.qualified_id });
                  }}
                >
                  <PluginIcon pluginId={plugin.id} icon={destination.icon} size={14} />
                  <span>{destination.title}</span>
                </button>
              ))}
            </Box>
          )}
        </Box>
      )}
      {button(
        "center",
        messages.pluginCenter,
        <SfIcon name="sf:gearshape" size={20} />,
        selection.kind === "center",
        () => onSelect({ kind: "center" })
      )}
    </Flex>
  );
}
