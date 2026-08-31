// 插件菜单贡献点的非组件部分。
//
// 与 `PluginMenus.tsx` 分开只为一条工程约束：一个文件同时导出组件和普通函数时
// React Fast Refresh 会失效。位置白名单本身在 Rust 侧（`plugin/manifest.rs` 的
// PluginMenuLocation）定义，与 Xedit 共享同一份 schema。

import { useCallback, useEffect, useState } from "react";
import { executePluginCommand, type CommandResponse, type PluginView } from "./plugins";

export type MenuLocation =
  | "commandPalette"
  | "session.context"
  | "composer.more"
  | "plugin.sidebar.row.context"
  | "chat.selection";

export type MenuEntry = {
  pluginId: string;
  pluginName: string;
  commandId: string;
  title: string;
  icon: string | null;
};

/** 某个位置上，所有已启用插件贡献的命令，按插件安装顺序排列。 */
export function menuEntries(plugins: PluginView[], location: MenuLocation): MenuEntry[] {
  return plugins
    .filter((plugin) => plugin.enabled)
    .flatMap((plugin) =>
      (plugin.menus[location] ?? []).flatMap((commandId) => {
        const command = plugin.commands.find((item) => item.id === commandId);
        return command
          ? [{ pluginId: plugin.id, pluginName: plugin.name, commandId, title: command.title, icon: command.icon }]
          : [];
      })
    );
}

/** 执行一条菜单命令，并把跳转类结果交给调用方消化。 */
export function usePluginCommandRunner(onNavigate: (qualifiedId: string) => void) {
  return useCallback(
    async (entry: MenuEntry, args: Record<string, string> = {}): Promise<CommandResponse> => {
      const response = await executePluginCommand(entry.pluginId, entry.commandId, args);
      if (response.destination) onNavigate(response.destination);
      return response;
    },
    [onNavigate]
  );
}

type SelectionState = { text: string; x: number; y: number } | null;

/**
 * 监听一个容器里的文字选中。
 *
 * 只认容器内的选中，且选中折叠（点一下取消选中）时立刻清掉——否则气泡会挂在
 * 屏幕上不走。选中的原文只做 trim 与长度上限，不改内容本身：插件拿到的必须是
 * 用户真正看到的那段字。
 */
export function useChatSelection(containerRef: React.RefObject<HTMLElement | null>, enabled: boolean) {
  const [selection, setSelection] = useState<SelectionState>(null);

  useEffect(() => {
    if (!enabled) {
      setSelection(null);
      return;
    }
    const onSelectionChange = () => {
      const active = window.getSelection();
      const container = containerRef.current;
      if (!active || active.isCollapsed || !container) {
        setSelection(null);
        return;
      }
      const anchor = active.anchorNode;
      if (!anchor || !container.contains(anchor)) {
        setSelection(null);
        return;
      }
      const text = active.toString().trim();
      if (!text) {
        setSelection(null);
        return;
      }
      const rect = active.getRangeAt(0).getBoundingClientRect();
      setSelection({ text: text.slice(0, 4000), x: rect.left, y: rect.bottom + 6 });
    };
    document.addEventListener("selectionchange", onSelectionChange);
    return () => document.removeEventListener("selectionchange", onSelectionChange);
  }, [containerRef, enabled]);

  return [selection, setSelection] as const;
}
