// The console's words, Chinese or English, picked from the browser's language with a switch.
// The English strings' keys type the Chinese ones, so a missing string fails `npm run check`.
import { en, type Key } from "../i18n/en";
import { zh } from "../i18n/zh";

export type Lang = "zh" | "en";

const STORE = "pinakes.lang";

function initial(): Lang {
  try {
    const saved = localStorage.getItem(STORE);
    if (saved === "zh" || saved === "en") return saved;
  } catch {
    // storage blocked: fall back to the browser's language
  }
  return navigator.language.toLowerCase().startsWith("zh") ? "zh" : "en";
}

class I18n {
  lang = $state<Lang>(initial());

  set(lang: Lang): void {
    this.lang = lang;
    document.documentElement.lang = lang === "zh" ? "zh-CN" : "en";
    try {
      localStorage.setItem(STORE, lang);
    } catch {
      // not remembered, still switched
    }
  }
}

export const i18n = new I18n();
document.documentElement.lang = i18n.lang === "zh" ? "zh-CN" : "en";

/** A string in the current language, `{name}` placeholders filled from `vars`. */
export function t(key: Key, vars?: Record<string, string | number>): string {
  const table = i18n.lang === "zh" ? zh : en;
  let s: string = table[key];
  if (vars) {
    for (const [k, v] of Object.entries(vars)) s = s.replaceAll(`{${k}}`, String(v));
  }
  return s;
}

/** Whether a string is a key of the console's words (for keys built from API values). */
export function isKey(k: string): k is Key {
  return k in en;
}
