import { useCallback, useEffect, useState } from "react";
import { Box, Button, Dialog, Flex, Input, NativeSelect, Portal, Text } from "@chakra-ui/react";
import type { Messages } from "./i18n";

type ProviderOption = { id: string; provider: string; model: string };
type ProfileSetting = {
  id: string;
  provider_profile: string | null;
  model: string | null;
  context_window: number;
  automatic: boolean;
  effective_provider: string;
  effective_model: string;
  recommended_model: string | null;
};
type Settings = {
  revision: string;
  default_provider: string;
  active_provider_override: string | null;
  root_model: string;
  small_model_routing: boolean;
  auto_dispatch_read_only: boolean;
  max_deep_calls_per_harness: number;
  providers: ProviderOption[];
  profiles: ProfileSetting[];
};

/// 可以自带触发按钮，也可以由外部控制开关。侧栏把它收进设置面板后走的是
/// 后一种：`open` 一给，这里就不再渲染自己的按钮。
export function ModelRoutingSettings({
  messages: t,
  open: controlledOpen,
  onOpenChange,
}: {
  messages: Messages;
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
}) {
  const [uncontrolledOpen, setUncontrolledOpen] = useState(false);
  const controlled = controlledOpen !== undefined;
  const open = controlled ? controlledOpen : uncontrolledOpen;
  const setOpen = (next: boolean) => {
    if (controlled) onOpenChange?.(next);
    else setUncontrolledOpen(next);
  };
  const [settings, setSettings] = useState<Settings | null>(null);
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const [saved, setSaved] = useState(false);

  const load = useCallback(async () => {
    setLoading(true); setError(""); setSaved(false);
    try {
      const response = await fetch("/api/settings/model-routing");
      const value = await response.json() as Settings & { error?: string };
      if (!response.ok) throw new Error(value.error || response.statusText);
      setSettings(value);
    } catch (reason) {
      setError(`${t.routingLoadFailed}: ${reason instanceof Error ? reason.message : String(reason)}`);
    } finally { setLoading(false); }
  }, [t.routingLoadFailed]);

  useEffect(() => { if (open) void load(); }, [load, open]);

  function updateProfile(index: number, patch: Partial<ProfileSetting>) {
    setSaved(false);
    setSettings((current) => current ? {
      ...current,
      profiles: current.profiles.map((profile, itemIndex) => {
        if (itemIndex !== index) return profile;
        const providerProfile = Object.hasOwn(patch, "provider_profile") ? patch.provider_profile ?? null : profile.provider_profile;
        const model = Object.hasOwn(patch, "model") ? patch.model ?? null : profile.model;
        return { ...profile, ...patch, automatic: providerProfile === null && model === null };
      }),
    } : current);
  }

  async function save() {
    if (!settings) return;
    setSaving(true); setError(""); setSaved(false);
    try {
      const response = await fetch("/api/settings/model-routing", {
        method: "PUT",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          revision: settings.revision,
          default_provider: settings.default_provider,
          root_model: settings.root_model,
          small_model_routing: settings.small_model_routing,
          auto_dispatch_read_only: settings.auto_dispatch_read_only,
          max_deep_calls_per_harness: settings.max_deep_calls_per_harness,
          profiles: settings.profiles.map(({ id, provider_profile, model, context_window }) => ({ id, provider_profile, model, context_window })),
        }),
      });
      const value = await response.json() as Settings & { error?: string };
      if (!response.ok) {
        if (response.status === 409) throw new Error(t.routingStale);
        throw new Error(value.error || response.statusText);
      }
      setSettings(value); setSaved(true);
    } catch (reason) {
      setError(`${t.routingSaveFailed}: ${reason instanceof Error ? reason.message : String(reason)}`);
    } finally { setSaving(false); }
  }

  function profileLabel(id: string) {
    const labels: Record<string, string> = {
      scout: t.routingProfileScout, reader: t.routingProfileReader, editor: t.routingProfileEditor,
      implementer: t.routingProfileImplementer, test_fixer: t.routingProfileTestFixer,
      tester: t.routingProfileTester, ops_runner: t.routingProfileOpsRunner, judge: t.routingProfileJudge,
      build_fixer: t.routingProfileBuildFixer, log_inspector: t.routingProfileLogInspector,
      git_detective: t.routingProfileGitDetective, deep: t.routingProfileDeep,
    };
    return labels[id] || t.unknownValue;
  }

  return <>
    {/* 受控时按钮由调用方提供。Chakra 的 outline 变体在这套深底上把文字压到
        近乎看不见，所以这里显式给前景与描边色，别依赖主题默认值。 */}
    {!controlled && <Button size="sm" variant="outline" width="100%" mb="5" color="#d8e2ec" borderColor="#3a4859" _hover={{ bg: "#16212c", color: "#f2f6fa", borderColor: "#4c5d72" }} onClick={() => setOpen(true)}>{t.modelRouting}</Button>}
    <Dialog.Root open={open} onOpenChange={(details) => setOpen(details.open)} size="xl">
      <Portal>
        <Dialog.Backdrop bg="#000c" />
        <Dialog.Positioner>
          <Dialog.Content className="routing-dialog" bg="#0d151e" color="#e7edf4" border="1px solid" borderColor="#33465a" borderRadius="12px">
            <Dialog.Header><Dialog.Title>{t.modelRouting}</Dialog.Title></Dialog.Header>
            <Dialog.Body>
              {loading && <Text color="#8290a3">{t.routingLoading}</Text>}
              {error && <Text color="#ff8f8f" mb="3">{error}</Text>}
              {settings && <>
                <Text fontSize="sm" color="#8290a3" mb="4">{t.routingApplyHint}</Text>
                {settings.active_provider_override && <Text className="routing-warning">{t.routingOverrideHint}: {settings.active_provider_override}</Text>}
                <Box className="routing-grid routing-root-grid">
                  <label><span>{t.routingRootProvider}</span><NativeSelect.Root><NativeSelect.Field value={settings.default_provider} onChange={(event) => {
                    const provider = settings.providers.find((item) => item.id === event.target.value);
                    setSettings({ ...settings, default_provider: event.target.value, root_model: provider?.model || settings.root_model }); setSaved(false);
                  }}>{settings.providers.map((provider) => <option key={provider.id} value={provider.id}>{provider.id} · {provider.provider}</option>)}</NativeSelect.Field><NativeSelect.Indicator /></NativeSelect.Root></label>
                  <label><span>{t.routingRootModel}</span><Input value={settings.root_model} onChange={(event) => { setSettings({ ...settings, root_model: event.target.value }); setSaved(false); }} /></label>
                  <label><span>{t.routingDeepBudget}</span><Input type="number" min={0} max={16} value={settings.max_deep_calls_per_harness} onChange={(event) => { setSettings({ ...settings, max_deep_calls_per_harness: Number(event.target.value) }); setSaved(false); }} /></label>
                </Box>
                <Flex gap="5" my="4" wrap="wrap">
                  <label className="routing-check"><input type="checkbox" checked={settings.small_model_routing} onChange={(event) => { setSettings({ ...settings, small_model_routing: event.target.checked }); setSaved(false); }} />{t.routingEnabled}</label>
                  <label className="routing-check"><input type="checkbox" checked={settings.auto_dispatch_read_only} onChange={(event) => { setSettings({ ...settings, auto_dispatch_read_only: event.target.checked }); setSaved(false); }} />{t.routingAutoDispatch}</label>
                </Flex>
                <Box overflowX="auto"><table className="routing-table"><thead><tr><th>{t.routingProfile}</th><th>{t.routingRecommended}</th><th>{t.routingProvider}</th><th>{t.routingModel}</th><th>{t.routingContext}</th><th>{t.routingEffective}</th></tr></thead><tbody>
                  {settings.profiles.map((profile, index) => <tr key={profile.id}>
                    <td>{profileLabel(profile.id)}</td>
                    <td><input aria-label={`${t.routingRecommended} ${profileLabel(profile.id)}`} type="checkbox" checked={profile.automatic} onChange={(event) => updateProfile(index, event.target.checked ? { provider_profile: null, model: null, automatic: true } : { model: profile.effective_model, automatic: false })} /></td>
                    <td><select value={profile.provider_profile || ""} onChange={(event) => updateProfile(index, { provider_profile: event.target.value || null })}><option value="">{t.routingInheritRoot}</option>{settings.providers.map((provider) => <option key={provider.id} value={provider.id}>{provider.id}</option>)}</select></td>
                    <td><input value={profile.model || ""} placeholder={profile.recommended_model || t.routingProviderDefault} onChange={(event) => updateProfile(index, { model: event.target.value.trim() ? event.target.value : null })} /></td>
                    <td><input type="number" min={4000} max={1000000} value={profile.context_window} onChange={(event) => updateProfile(index, { context_window: Number(event.target.value) })} /></td>
                    <td><Text fontSize="xs" color="#8fa4ba">{profile.effective_provider} · {profile.effective_model}</Text></td>
                  </tr>)}
                </tbody></table></Box>
              </>}
            </Dialog.Body>
            <Dialog.Footer><Flex gap="2" align="center"><Text fontSize="sm" color="#74d69a">{saved ? t.routingSaved : ""}</Text><Button variant="ghost" onClick={() => setOpen(false)}>{t.cancel}</Button><Button disabled={!settings || saving || loading} onClick={() => void save()}>{saving ? t.routingSaving : t.routingSave}</Button></Flex></Dialog.Footer>
          </Dialog.Content>
        </Dialog.Positioner>
      </Portal>
    </Dialog.Root>
  </>;
}
