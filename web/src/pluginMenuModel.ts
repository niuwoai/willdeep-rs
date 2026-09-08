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
 * 在选择手势结束或右键时保存快照。菜单拥有快照直到明确关闭，不能因为
 * 浏览器焦点切换、轮询刷新或菜单按下造成 selectionchange 就销毁它。
 */
export function useChatSelection(containerRef: React.RefObject<HTMLElement | null>, enabled: boolean) {
  const [selection, setSelection] = useState<SelectionState>(null);

  useEffect(() => {
    if (!enabled) {
      setSelection(null);
      return;
    }
    const readSelection = (): SelectionState => {
      const active = window.getSelection();
      const container = containerRef.current;
      if (!active || active.isCollapsed || active.rangeCount === 0 || !container) return null;
      const range = active.getRangeAt(0);
      if (!container.contains(range.commonAncestorContainer)) return null;
      const text = active.toString().trim();
      if (!text) return null;
      const rect = range.getBoundingClientRect();
      return { text: text.slice(0, 4000), x: rect.left, y: rect.bottom + 6 };
    };
    const onPointerUp = (event: PointerEvent) => {
      if (event.button === 0 && containerRef.current?.contains(event.target as Node)) setSelection(readSelection());
    };
    const onContextMenu = (event: MouseEvent) => {
      if (!containerRef.current?.contains(event.target as Node)) return;
      const selected = readSelection();
      if (!selected) return;
      event.preventDefault();
      setSelection({ ...selected, x: event.clientX, y: event.clientY });
    };
    const onKeyUp = (event: KeyboardEvent) => {
      if (event.key === "Shift" || event.shiftKey) {
        const selected = readSelection();
        if (selected) setSelection(selected);
      }
    };
    document.addEventListener("pointerup", onPointerUp);
    document.addEventListener("contextmenu", onContextMenu);
    document.addEventListener("keyup", onKeyUp);
    return () => {
      document.removeEventListener("pointerup", onPointerUp);
      document.removeEventListener("contextmenu", onContextMenu);
      document.removeEventListener("keyup", onKeyUp);
    };
  }, [containerRef, enabled]);

  return [selection, setSelection] as const;
}
