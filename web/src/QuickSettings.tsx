import { Box, Flex, NativeSelect, Text } from "@chakra-ui/react";
import { languageLabels, languages, type Language, type Messages } from "./i18n";
import { themeModes, type ThemeMode } from "./theme";

/// 右下角常驻的两个小开关：语言与主题。
///
/// 语言在左栏设置里也有一份，这里再放一个是因为**用得最多的开关不该藏在抽屉
/// 里**；两处改的是同一个状态，不会各说各话。
type Props = {
  messages: Messages;
  language: Language;
  onLanguageChange: (language: Language) => void;
  theme: ThemeMode;
  onThemeChange: (theme: ThemeMode) => void;
};

export function QuickSettings({ messages: t, language, onLanguageChange, theme, onThemeChange }: Props) {
  const themeLabels: Record<ThemeMode, string> = {
    system: t.themeSystem,
    dark: t.themeDark,
    light: t.themeLight,
  };
  return <Flex className="quick-settings" gap="3" align="flex-end">
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
  </Flex>;
}
