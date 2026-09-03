import { Box, Flex, NativeSelect, Text } from "@chakra-ui/react";
import { languageLabels, languages, type Language, type Messages } from "./i18n";

/// 右下角常驻的两个小开关。
///
/// 语言在左栏设置里也有一份,这里再放一个是因为**用得最多的两个开关不该藏在
/// 抽屉里**;两处改的是同一个状态,不会各说各话。
type Props = {
  messages: Messages;
  language: Language;
  onLanguageChange: (language: Language) => void;
};

export function QuickSettings({ messages: t, language, onLanguageChange }: Props) {
  return <Flex className="quick-settings" gap="2" align="center">
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
  </Flex>;
}
