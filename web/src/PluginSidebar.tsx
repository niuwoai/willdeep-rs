// 声明式侧栏：把插件给的 JSON 组件树原生渲染出来。
//
// 这里刻意**不**执行任何插件脚本、不接受 CSS、不接受坐标——文档已经在 Rust
// 侧按共享 schema 校验过（组件数、嵌套深度、命令引用、progress 范围）。
// 渲染器的职责只剩一件：认识的 kind 画出来，不认识的跳过而不是炸掉。

import { useCallback, useEffect, useState } from "react";
import { Box, Flex, Text } from "@chakra-ui/react";
import type { Messages } from "./i18n";
import { SfIcon } from "./sfSymbols";
import {
  executePluginCommand,
  fetchSidebar,
  type DeclarativeComponent,
  type PluginDestinationView,
  type PluginView,
  type SidebarDocument,
} from "./plugins";

type Props = {
  plugin: PluginView;
  destination: PluginDestinationView;
  locale: string;
  messages: Messages;
  reloadToken: number;
  onNavigate: (qualifiedDestination: string) => void;
  onSelectItem: (itemId: string | null) => void;
  selectedItemId: string | null;
  /// 行右键：宿主弹菜单。`commands` 是这一行自己声明的 contextCommands，
  /// 为空时退回该插件在 `plugin.sidebar.row.context` 上贡献的全部命令。
  onRowContextMenu?: (event: React.MouseEvent, componentId: string, commands: string[]) => void;
};

export function PluginSidebar({
  plugin,
  destination,
  locale,
  messages,
  reloadToken,
  onNavigate,
  onSelectItem,
  selectedItemId,
  onRowContextMenu,
}: Props) {
  const [document, setDocument] = useState<SidebarDocument | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const sidebarId = destination.sidebar?.id;

  useEffect(() => {
    if (!sidebarId) return;
    let cancelled = false;
    setLoading(true);
    fetchSidebar(plugin.id, sidebarId, locale)
      .then((value) => {
        if (cancelled) return;
        setDocument(value);
        setError(null);
      })
      .catch((reason: unknown) => {
        if (cancelled) return;
        setError(reason instanceof Error ? reason.message : String(reason));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [plugin.id, sidebarId, locale, reloadToken]);

  const runCommand = useCallback(
    async (commandId: string, args?: Record<string, string>) => {
      try {
        const response = await executePluginCommand(plugin.id, commandId, args ?? {});
        if (response.destination) onNavigate(response.destination);
      } catch (reason) {
        setError(reason instanceof Error ? reason.message : String(reason));
      }
    },
    [plugin.id, onNavigate]
  );

  if (!sidebarId) return null;

  const strings = document?.strings ?? {};
  const text = (key: string | undefined) => (key ? strings[key] ?? key : "");

  const render = (component: DeclarativeComponent, depth: number) => {
    const key = component.id;
    const children = component.children ?? [];
    switch (component.kind) {
      case "section":
        return (
          <Box key={key} className="plugin-sidebar-section">
            {component.titleKey && <Text className="plugin-sidebar-heading">{text(component.titleKey)}</Text>}
            {children.map((child) => render(child, depth + 1))}
          </Box>
        );
      case "list":
        return (
          <Box key={key} className="plugin-sidebar-list">
            {children.map((child) => render(child, depth + 1))}
          </Box>
        );
      case "row": {
        const selected = selectedItemId === component.id;
        const clickable = Boolean(component.command);
        return (
          <Flex
            key={key}
            className={`plugin-sidebar-row${selected ? " selected" : ""}${clickable ? " clickable" : ""}`}
            onClick={() => {
              onSelectItem(component.id);
              if (component.command) void runCommand(component.command, component.arguments);
            }}
            onContextMenu={(event) => {
              onSelectItem(component.id);
              onRowContextMenu?.(event, component.id, component.contextCommands ?? []);
            }}
          >
            {children.map((child) => render(child, depth + 1))}
          </Flex>
        );
      }
      case "label":
        return (
          <Text key={key} className="plugin-sidebar-label">
            {component.systemImage && <SfIcon name={component.systemImage} size={14} />}
            <span>{text(component.titleKey) || component.value || ""}</span>
            {component.subtitleKey && <small>{text(component.subtitleKey)}</small>}
          </Text>
        );
      case "icon":
        return <SfIcon key={key} name={component.systemImage} size={14} />;
      case "badge":
        return (
          <span key={key} className="plugin-sidebar-badge">
            {component.value ?? text(component.titleKey)}
          </span>
        );
      case "status":
        return (
          <span key={key} className="plugin-sidebar-status">
            {component.value ?? text(component.titleKey)}
          </span>
        );
      case "progress": {
        const value = Math.max(0, Math.min(1, component.progress ?? 0));
        return (
          <Box key={key} className="plugin-sidebar-progress" role="progressbar" aria-valuenow={Math.round(value * 100)}>
            <Box className="plugin-sidebar-progress-fill" style={{ width: `${value * 100}%` }} />
          </Box>
        );
      }
      case "button":
        return (
          <button
            key={key}
            type="button"
            className="plugin-sidebar-button"
            onClick={() => component.command && void runCommand(component.command, component.arguments)}
          >
            {component.systemImage && <SfIcon name={component.systemImage} size={14} />}
            {text(component.titleKey) || component.value}
          </button>
        );
      case "disclosure":
        return (
          <details key={key} className="plugin-sidebar-disclosure">
            <summary>{text(component.titleKey)}</summary>
            {children.map((child) => render(child, depth + 1))}
          </details>
        );
      case "separator":
        return <Box key={key} className="plugin-sidebar-separator" />;
      case "emptyState":
      case "loadingState":
      case "errorState":
        return (
          <Text key={key} className={`plugin-sidebar-state ${component.kind}`}>
            {text(component.titleKey) || component.value}
          </Text>
        );
      default:
        return null;
    }
  };

  return (
    <Box as="aside" className="plugin-sidebar">
      <Text className="plugin-sidebar-title">{plugin.name}</Text>
      {loading && !document && <Text className="plugin-sidebar-state">{messages.pluginSidebarLoading}</Text>}
      {/* 动态 Resource 读失败时后端会回落到包内 Schema。数据是旧的这件事必须
          说出来——一个装作正常的过期面板比一个报错的面板更容易误导人。 */}
      {document?.degraded && (
        <Text className="plugin-sidebar-state errorState">{messages.pluginSidebarStale}</Text>
      )}
      {error && <Text className="plugin-sidebar-state errorState">{error}</Text>}
      {document?.document.components.map((component) => render(component, 0))}
    </Box>
  );
}
