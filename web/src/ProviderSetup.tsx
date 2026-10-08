import { useState } from "react";
import { Box, Button, Flex, Text } from "@chakra-ui/react";
import type { Messages } from "./i18n";
import type { PluginView } from "./plugins";

export type SetupStatus = { ready: boolean; ruby_available: boolean; config_path: string };

type Props = {
  status: SetupStatus | null;
  loadFailed?: boolean;
  plugin: PluginView | undefined;
  messages: Messages;
  onOpen: (destination: string | null) => void;
  onCheck: () => Promise<SetupStatus>;
  onDone: () => void;
};

export function ProviderSetup({ status, loadFailed, plugin, messages: t, onOpen, onCheck, onDone }: Props) {
  const [error, setError] = useState("");
  const [checking, setChecking] = useState(false);
  const destination = plugin?.enabled && !plugin.approval_gap ? plugin.destinations[0]?.qualified_id : null;
  const check = async () => {
    setChecking(true);
    setError("");
    try {
      const updated = await onCheck();
      if (updated.ready) onDone();
      else setError(t.setupIncomplete);
    } catch {
      setError(t.setupCheckFailed);
    } finally {
      setChecking(false);
    }
  };
  return <Box className="provider-setup" role="region" aria-label={t.setupTitle}>
    <Text fontWeight="semibold">{t.setupTitle}</Text>
    <Text>{destination ? t.setupConfigure : t.setupEnable}</Text>
    <Text>{t.setupSteps}</Text>
    {status && <Text fontSize="sm">{t.setupFile}: {status.config_path}</Text>}
    {status && !status.ruby_available && <Text role="alert">{t.setupRubyMissing}</Text>}
    {error && <Text role="alert">{error}</Text>}
    {loadFailed && <Text role="alert">{t.setupCheckFailed}</Text>}
    <Flex gap="2" mt="2" wrap="wrap">
      <Button size="sm" onClick={() => onOpen(destination ?? null)}>{destination ? t.setupOpenConfig : t.setupOpenPlugins}</Button>
      <Button size="sm" disabled={checking} onClick={() => void check()}>{checking ? t.setupChecking : t.setupDone}</Button>
      <Button size="sm" variant="ghost" onClick={onDone}>{t.setupLater}</Button>
    </Flex>
  </Box>;
}
