// 侧栏顶部的设置入口。
//
// 语言和模型路由是「设一次就不再动」的东西，之前它们占着侧栏最顶上的两块，
// 把每天都在用的会话列表挤到了下半屏。收进一个齿轮里：常用的留在外面，
// 偶尔用的点开再说。

import { useEffect, useRef, useState } from "react";
import { Box, NativeSelect, Text } from "@chakra-ui/react";
import { languageLabels, languages, type Language, type Messages } from "./i18n";
import { ModelRoutingSettings } from "./ModelRoutingSettings";
import { SfIcon } from "./sfSymbols";

type Props = {
  messages: Messages;
  language: Language;
  onLanguageChange: (language: Language) => void;
};

export function SidebarSettings({ messages, language, onLanguageChange }: Props) {
  const [open, setOpen] = useState(false);
  const [routingOpen, setRoutingOpen] = useState(false);
  const anchorRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!open) return;
    const dismiss = (event: MouseEvent) => {
      if (!anchorRef.current?.contains(event.target as Node)) setOpen(false);
    };
    const onKey = (event: KeyboardEvent) => event.key === "Escape" && setOpen(false);
    // 延迟一帧：打开面板的那一次点击还在往上冒泡，立刻挂监听会把它当场关掉。
    const armed = window.setTimeout(() => window.addEventListener("click", dismiss), 0);
    window.addEventListener("keydown", onKey);
    return () => {
      window.clearTimeout(armed);
      window.removeEventListener("click", dismiss);
      window.removeEventListener("keydown", onKey);
    };
  }, [open]);

  return (
    <Box ref={anchorRef} className="sidebar-settings">
      <button
        type="button"
        className={`sidebar-settings-button${open ? " active" : ""}`}
        title={messages.settings}
        aria-label={messages.settings}
        aria-expanded={open}
        onClick={() => setOpen((current) => !current)}
      >
        <SfIcon name="sf:gearshape" size={17} />
      </button>

      {open && (
        <Box className="sidebar-settings-panel">
          <Text className="sidebar-settings-label">{messages.language}</Text>
          <NativeSelect.Root size="sm" mb="3">
            <NativeSelect.Field
              aria-label={messages.language}
              value={language}
              onChange={(event) => onLanguageChange(event.target.value as Language)}
              bg="var(--bg-raised)"
              borderColor="var(--border)"
              color="var(--text)"
            >
              {languages.map((code) => (
                <option key={code} value={code}>
                  {languageLabels[code]}
                </option>
              ))}
            </NativeSelect.Field>
            <NativeSelect.Indicator />
          </NativeSelect.Root>

          <button
            type="button"
            className="sidebar-settings-item"
            onClick={() => {
              setOpen(false);
              setRoutingOpen(true);
            }}
          >
            {messages.modelRouting}
            <span aria-hidden="true">›</span>
          </button>
        </Box>
      )}

      {/* 模型路由那个大对话框仍归 ModelRoutingSettings 自己管，这里只负责把它打开。 */}
      <ModelRoutingSettings messages={messages} open={routingOpen} onOpenChange={setRoutingOpen} />
    </Box>
  );
}
