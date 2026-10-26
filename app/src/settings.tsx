import { ReactNode, createContext, useContext, useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { AI_MODELS, AiModel } from "./api";
import { Messages, en } from "./i18n/en";
import { ko } from "./i18n/ko";

export type Locale = "ko" | "en";
export type Theme = "system" | "light" | "dark";

export interface Settings {
  locale: Locale;
  theme: Theme;
  aiModel: AiModel;
}

/** Each language's name in that language, for the picker. */
export const LOCALES: { value: Locale; name: string }[] = [
  { value: "ko", name: "한국어" },
  { value: "en", name: "English" },
];

/** Each language's English name, for asking the AI to answer in it. */
export const LANGUAGE_NAMES: Record<Locale, string> = { ko: "Korean", en: "English" };

const CATALOGS: Record<Locale, Messages> = { ko, en };
const DEFAULTS: Settings = { locale: "ko", theme: "system", aiModel: AI_MODELS[0] };
const STORAGE_KEY = "dolmen.settings";

interface SettingsValue {
  settings: Settings;
  update: (change: Partial<Settings>) => void;
}

const SettingsContext = createContext<SettingsValue>({ settings: DEFAULTS, update: () => {} });
const MessagesContext = createContext<Messages>(CATALOGS[DEFAULTS.locale]);

/** Holds the app's settings, keeps them across launches, and applies them. */
export function SettingsProvider({ children }: { children: ReactNode }) {
  const [settings, setSettings] = useState(load);

  useEffect(() => {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(settings));
    } catch {
      // Storage can be unavailable; the settings then last until the app quits.
    }
    const root = document.documentElement;
    root.lang = settings.locale;
    if (settings.theme === "system") delete root.dataset.theme;
    else root.dataset.theme = settings.theme;
    // The native title bar follows too; outside Tauri (a browser preview) there is none.
    try {
      getCurrentWindow()
        .setTheme(settings.theme === "system" ? null : settings.theme)
        .catch(() => {});
    } catch {
      // Not running in Tauri.
    }
  }, [settings]);

  const update = (change: Partial<Settings>) => setSettings((current) => ({ ...current, ...change }));

  return (
    <SettingsContext.Provider value={{ settings, update }}>
      <MessagesContext.Provider value={CATALOGS[settings.locale]}>{children}</MessagesContext.Provider>
    </SettingsContext.Provider>
  );
}

export function useSettings() {
  return useContext(SettingsContext);
}

/** The messages in the chosen language. */
export function useT() {
  return useContext(MessagesContext);
}

function load(): Settings {
  try {
    const stored = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "{}");
    return {
      locale: LOCALES.some((l) => l.value === stored.locale) ? stored.locale : DEFAULTS.locale,
      theme: ["system", "light", "dark"].includes(stored.theme) ? stored.theme : DEFAULTS.theme,
      aiModel: AI_MODELS.includes(stored.aiModel) ? stored.aiModel : DEFAULTS.aiModel,
    };
  } catch {
    return DEFAULTS;
  }
}
