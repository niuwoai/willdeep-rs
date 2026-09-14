import { Box, Flex, NativeSelect, Text } from "@chakra-ui/react";
import { useEffect, useRef, useState } from "react";
import { SfIcon } from "./sfSymbols";
import { languageLabels, languages, type Language, type Messages } from "./i18n";
import { themeModes, type ThemeMode } from "./theme";

/// 输入区下方只保留图标，语言与主题在点击后展开。
type Props = {
  messages: Messages;
  language: Language;
  onLanguageChange: (language: Language) => void;
  theme: ThemeMode;
  onThemeChange: (theme: ThemeMode) => void;
};

export function QuickSettings({ messages: t, language, onLanguageChange, theme, onThemeChange }: Props) {
  const [open, setOpen] = useState(false);
  const anchor = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (!open) return;
    const dismiss = (event: PointerEvent) => {
      if (!anchor.current?.contains(event.target as Node)) setOpen(false);
    };
    const escape = (event: KeyboardEvent) => {
      if (event.key === "Escape") { setOpen(false); trigger.current?.focus(); }
    };
    document.addEventListener("pointerdown", dismiss);
    document.addEventListener("keydown", escape);
    return () => {
      document.removeEventListener("pointerdown", dismiss);
      document.removeEventListener("keydown", escape);
    };
  }, [open]);
  const themeLabels: Record<ThemeMode, string> = {
    system: t.themeSystem,
    dark: t.themeDark,
    light: t.themeLight,
  };
  return <Box className="quick-settings-anchor" ref={anchor}>
    <button ref={trigger} type="button" className="quick-settings-trigger" aria-label={t.quickSettings} title={t.quickSettings} aria-expanded={open} aria-controls="quick-settings-panel" onClick={() => setOpen((value) => !value)}>
      <SfIcon name="sf:gearshape" size={18} />
    </button>
    {open && <Flex id="quick-settings-panel" role="group" aria-label={t.quickSettings} className="quick-settings quick-settings-panel" gap="3" align="flex-end">
    <Box>
      <Text className="quick-settings-label">{t.language}</Text>
      <NativeSelect.Root size="xs">
        <NativeSelect.Field
          aria-label={t.language}
          value={language}
          onChange={(event) => onLanguageChange(event.target.value as Language)}
          className="quick-settings-field"
        >
          {languages.map((code) => <option key={code} value={code}>{languageLabels[code]}</option>)}
        </NativeSelect.Field>
        <NativeSelect.Indicator />
      </NativeSelect.Root>
    </Box>
    <Box>
      <Text className="quick-settings-label">{t.theme}</Text>
      <NativeSelect.Root size="xs">
        <NativeSelect.Field
          aria-label={t.theme}
          value={theme}
          onChange={(event) => onThemeChange(event.target.value as ThemeMode)}
          className="quick-settings-field"
        >
          {themeModes.map((mode) => <option key={mode} value={mode}>{themeLabels[mode]}</option>)}
        </NativeSelect.Field>
        <NativeSelect.Indicator />
      </NativeSelect.Root>
    </Box>
  </Flex>}
  </Box>;
}
