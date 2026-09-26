// 插件页面的标题栏。
//
// 布局照 macOS 宿主 `AgentPluginPageHost.toolbar`：目的地图标 + 标题在左，
// 右边依次是清单声明的工具栏命令、刷新、「…」菜单；高 44（AgentDesign.
// headerHeight），左右内边距 16，下面一条分隔线。
//
// 「…」菜单里 macOS 还有一项「在 Finder 中显示」——浏览器够不着服务端的
// 文件系统，这里只保留「设置」（跳插件中心）。

import { useEffect, useRef, useState } from "react";
import type { Messages } from "./i18n";
import type { PluginCommandView } from "./plugins";
import { PluginIcon, SfIcon } from "./sfSymbols";

type Props = {
  pluginId: string;
  icon: string | null;
  title: string;
  commands: PluginCommandView[];
  busyCommand: string | null;
  messages: Messages;
  onRunCommand: (commandId: string) => void;
  onRefresh: () => void;
  onOpenSettings: () => void;
};

export function PluginPageHeader({
  pluginId,
  icon,
  title,
  commands,
  busyCommand,
  messages,
  onRunCommand,
  onRefresh,
  onOpenSettings,
}: Props) {
  const [menuOpen, setMenuOpen] = useState(false);
  const menuRef = useRef<HTMLDivElement | null>(null);

  // 点外面或按 Esc 收起。
  useEffect(() => {
    if (!menuOpen) return;
    const dismiss = (event: PointerEvent) => {
      if (!menuRef.current?.contains(event.target as Node)) setMenuOpen(false);
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setMenuOpen(false);
    };
    window.addEventListener("pointerdown", dismiss);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("pointerdown", dismiss);
      window.removeEventListener("keydown", onKey);
    };
  }, [menuOpen]);

  return (
    <header className="plugin-toolbar">
      <span className="plugin-title-icon">
        <PluginIcon pluginId={pluginId} icon={icon} size={16} />
      </span>
      <h1 className="plugin-title" title={title}>
        {title}
      </h1>
      <div className="plugin-toolbar-actions">
        {commands.map((command) => (
          <button
            key={command.id}
            type="button"
            className="plugin-toolbar-command"
            title={command.title}
            aria-label={command.title}
            disabled={busyCommand === command.id}
            onClick={() => onRunCommand(command.id)}
          >
            <PluginIcon pluginId={pluginId} icon={command.icon} kind="command" size={15} />
            <span className="plugin-toolbar-command-label">{command.title}</span>
          </button>
        ))}
        <button
          type="button"
          className="plugin-toolbar-button"
          title={messages.pluginRefresh}
          aria-label={messages.pluginRefresh}
          onClick={onRefresh}
        >
          <SfIcon name="sf:arrow.clockwise" size={16} />
        </button>
        <div className="plugin-toolbar-menu" ref={menuRef}>
          <button
            type="button"
            className={`plugin-toolbar-button${menuOpen ? " active" : ""}`}
            title={messages.pluginMoreActions}
            aria-label={messages.pluginMoreActions}
            aria-haspopup="menu"
            aria-expanded={menuOpen}
            onClick={() => setMenuOpen((current) => !current)}
          >
            <SfIcon name="sf:ellipsis" size={16} />
          </button>
          {menuOpen && (
            <div className="plugin-toolbar-menu-list" role="menu">
              <button
                type="button"
                role="menuitem"
                className="plugin-popup-item"
                onClick={() => {
                  setMenuOpen(false);
                  onOpenSettings();
                }}
              >
                <SfIcon name="sf:gearshape" size={14} />
                <span>{messages.pluginSettings}</span>
              </button>
            </div>
          )}
        </div>
      </div>
    </header>
  );
}
