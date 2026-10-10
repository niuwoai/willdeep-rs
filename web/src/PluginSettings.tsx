import { useState } from "react";
import type { Messages } from "./i18n";
import { setPluginSetting, type PluginView } from "./plugins";

type Props = {
  plugin: PluginView;
  messages: Messages;
  onChanged: () => void;
};

export function PluginSettings({ plugin, messages, onChanged }: Props) {
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const dirty = Object.keys(drafts).length > 0;

  function edit(key: string, value: string, original: string) {
    setDrafts((previous) => {
      const next = { ...previous };
      if (value === original) delete next[key];
      else next[key] = value;
      return next;
    });
    setSaved(false);
    setError(null);
  }

  async function save() {
    if (saving || !dirty) return;
    setSaving(true);
    setError(null);
    try {
      for (const [key, value] of Object.entries(drafts)) {
        await setPluginSetting(plugin.id, key, value || null);
        setDrafts((previous) => {
          const next = { ...previous };
          delete next[key];
          return next;
        });
      }
      setSaved(true);
    } catch {
      // 不展示原始后端错误，以免凭据意外进入界面。
      setError(messages.pluginSettingsSaveFailed);
    } finally {
      setSaving(false);
      onChanged();
    }
  }

  return (
    <form className="plugin-settings" onSubmit={(event) => { event.preventDefault(); void save(); }}>
      <fieldset disabled={saving} className="plugin-settings-fields">
        {plugin.settings.map((setting) => {
          const original = setting.type === "secret" ? "" : setting.value ?? "";
          const value = drafts[setting.id] ?? original;
          return (
            <div key={setting.id} className="plugin-setting-row">
              <div className="plugin-setting-label">
                <label className="plugin-setting-title" htmlFor={`${plugin.id}-${setting.id}`}>{setting.title}</label>
                {setting.description && <p className="plugin-setting-about">{setting.description}</p>}
              </div>
              {setting.type === "boolean" ? (
                <input
                  id={`${plugin.id}-${setting.id}`}
                  type="checkbox"
                  checked={(drafts[setting.id] ?? setting.value ?? setting.default_value) === "true"}
                  onChange={(event) => edit(setting.id, String(event.target.checked), setting.value ?? setting.default_value ?? "false")}
                />
              ) : (
                <input
                  id={`${plugin.id}-${setting.id}`}
                  className="plugin-setting-input"
                  type={setting.type === "secret" ? "password" : "text"}
                  autoComplete={setting.type === "secret" ? "new-password" : "off"}
                  placeholder={setting.type === "secret" && setting.configured ? messages.pluginSecretStored : setting.default_value ?? ""}
                  value={value}
                  onChange={(event) => edit(setting.id, event.target.value, original)}
                />
              )}
            </div>
          );
        })}
      </fieldset>
      <div className="plugin-settings-actions">
        <span role="status" aria-live="polite" className="plugin-setting-about">
          {saving ? messages.pluginSettingsSaving : dirty ? messages.pluginSettingsUnsaved : saved ? messages.pluginSettingsSaved : messages.pluginSettingsHint}
        </span>
        <button type="submit" className="plugin-primary-button plugin-settings-save" disabled={saving || !dirty}>
          {saving ? messages.pluginSettingsSaving : messages.pluginSettingsSave}
        </button>
      </div>
      {error && <p className="plugin-error" role="alert">{error}</p>}
    </form>
  );
}
