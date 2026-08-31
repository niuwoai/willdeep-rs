// 插件菜单贡献点的界面部分：命令面板、右键/气泡浮层。
//
// 目的地是插件的主入口，菜单是它的**顺手入口**——收藏夹和待办真正的用法是
// 「聊天里选中一句就能记下」，而不是先切到那个目的地再手打一遍。所以这几个
// 位置不是锦上添花，是那两个插件的一半价值所在。
//
// 非组件的部分（位置类型、命令收集、选中监听）在 `pluginMenuModel.ts`。

import { useEffect, useMemo, useRef, useState } from "react";
import { Box, Text } from "@chakra-ui/react";
import type { Messages } from "./i18n";
import { SfIcon } from "./sfSymbols";
import type { MenuEntry } from "./pluginMenuModel";

type PaletteProps = {
  entries: MenuEntry[];
  messages: Messages;
  onRun: (entry: MenuEntry) => void;
  onClose: () => void;
};

export function PluginCommandPalette({ entries, messages, onRun, onClose }: PaletteProps) {
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const inputRef = useRef<HTMLInputElement | null>(null);

  const matches = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (!needle) return entries;
    return entries.filter((entry) =>
      `${entry.title} ${entry.pluginName} ${entry.commandId}`.toLowerCase().includes(needle)
    );
  }, [entries, query]);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);
  useEffect(() => {
    setActive(0);
  }, [query]);

  return (
    <Box className="plugin-palette-backdrop" onMouseDown={onClose}>
      <Box className="plugin-palette" onMouseDown={(event) => event.stopPropagation()}>
        <input
          ref={inputRef}
          className="plugin-palette-input"
          value={query}
          placeholder={messages.pluginPalettePlaceholder}
          aria-label={messages.pluginPalette}
          onChange={(event) => setQuery(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Escape") { onClose(); return; }
            if (event.key === "ArrowDown") { event.preventDefault(); setActive((current) => Math.min(current + 1, matches.length - 1)); }
            if (event.key === "ArrowUp") { event.preventDefault(); setActive((current) => Math.max(current - 1, 0)); }
            if (event.key === "Enter" && matches[active]) { event.preventDefault(); onRun(matches[active]); }
          }}
        />
        <Box className="plugin-palette-list">
          {matches.map((entry, index) => (
            <button
              key={`${entry.pluginId}:${entry.commandId}`}
              type="button"
              className={`plugin-palette-item${index === active ? " active" : ""}`}
              onMouseEnter={() => setActive(index)}
              onClick={() => onRun(entry)}
            >
              <SfIcon name={entry.icon} size={15} />
              <span className="plugin-palette-title">{entry.title}</span>
              <span className="plugin-palette-source">{entry.pluginName}</span>
            </button>
          ))}
          {matches.length === 0 && <Text className="plugin-palette-empty">{messages.pluginPaletteEmpty}</Text>}
        </Box>
      </Box>
    </Box>
  );
}

type PopupProps = {
  entries: MenuEntry[];
  x: number;
  y: number;
  onRun: (entry: MenuEntry) => void;
  onClose: () => void;
};

/** 右键菜单与选中气泡共用的浮层。定位会夹到视口内，免得贴边时半截在屏幕外。 */
export function PluginMenuPopup({ entries, x, y, onRun, onClose }: PopupProps) {
  const ref = useRef<HTMLDivElement | null>(null);
  const [position, setPosition] = useState({ left: x, top: y });

  useEffect(() => {
    const node = ref.current;
    if (!node) return;
    const rect = node.getBoundingClientRect();
    setPosition({
      left: Math.max(8, Math.min(x, window.innerWidth - rect.width - 8)),
      top: Math.max(8, Math.min(y, window.innerHeight - rect.height - 8)),
    });
  }, [x, y]);

  // onClose 在 App 里是内联箭头函数，每次渲染都是新的。存进 ref，
  // 免得下面那个 effect 每渲染一次就重新注册一轮监听。
  const closeRef = useRef(onClose);
  closeRef.current = onClose;

  useEffect(() => {
    const dismiss = (event: MouseEvent) => {
      if (!ref.current?.contains(event.target as Node)) closeRef.current();
    };
    const onKey = (event: KeyboardEvent) => event.key === "Escape" && closeRef.current();
    // 延迟一帧再挂 click 监听。打开菜单的那一次点击还在往 window 冒泡，
    // 立刻挂上去的话它会把刚弹出来的菜单当场关掉——表现就是「点了没反应」。
    const armed = window.setTimeout(() => {
      window.addEventListener("click", dismiss);
      window.addEventListener("contextmenu", dismiss);
    }, 0);
    window.addEventListener("keydown", onKey);
    return () => {
      window.clearTimeout(armed);
      window.removeEventListener("click", dismiss);
      window.removeEventListener("contextmenu", dismiss);
      window.removeEventListener("keydown", onKey);
    };
  }, []);

  if (entries.length === 0) return null;

  return (
    <Box ref={ref} className="plugin-popup" style={{ left: position.left, top: position.top }} role="menu">
      {entries.map((entry) => (
        <button
          key={`${entry.pluginId}:${entry.commandId}`}
          type="button"
          role="menuitem"
          className="plugin-popup-item"
          onClick={() => onRun(entry)}
        >
          <SfIcon name={entry.icon} size={14} />
          <span>{entry.title}</span>
          <small>{entry.pluginName}</small>
        </button>
      ))}
    </Box>
  );
}
