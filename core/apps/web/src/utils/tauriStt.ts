export type SttAvailability = {
  available: boolean;
  reason?: string | null;
};

export type SttPermissionState = "granted" | "denied" | "unknown";

export type SttPermission = {
  microphone: SttPermissionState;
  speechRecognition: SttPermissionState;
};

export type SttResult = {
  transcript: string;
  isFinal: boolean;
  confidence?: number | null;
};

export type SttStateChange = {
  state: "idle" | "listening" | "processing";
};

export type SttError = {
  code: string;
  message: string;
  details?: unknown;
};

export type SttSupportedLanguage = {
  code: string;
  name: string;
  installed?: boolean;
};

export type SttSupportedLanguages = {
  languages: SttSupportedLanguage[];
};

export type SttDownloadProgress = {
  status?: string;
  model?: string;
  progress?: number;
};

export type SttApi = {
  isAvailable: () => Promise<SttAvailability>;
  getSupportedLanguages: () => Promise<SttSupportedLanguages>;
  checkPermission: () => Promise<SttPermission>;
  requestPermission: () => Promise<SttPermission>;
  startListening: (config?: {
    language?: string;
    interimResults?: boolean;
    continuous?: boolean;
    maxDuration?: number;
    onDevice?: boolean;
  }) => Promise<void>;
  stopListening: () => Promise<void>;
  onResult: (handler: (result: SttResult) => void) => Promise<() => void>;
  onStateChange: (handler: (event: SttStateChange) => void) => Promise<() => void>;
  onError: (handler: (error: SttError) => void) => Promise<() => void>;
};

export const loadSttApi = async (): Promise<SttApi | null> => {
  try {
    return (await import("tauri-plugin-stt-api")) as unknown as SttApi;
  } catch {
    return null;
  }
};

const TAURI_LANGUAGE_MAP: Record<string, string> = {
  en: "en-US",
  pt: "pt-BR",
  es: "es-ES",
  fr: "fr-FR",
  de: "de-DE",
  ru: "ru-RU",
  zh: "zh-CN",
  ja: "ja-JP",
  it: "it-IT",
};

export const normalizeTauriLanguage = (value: string | null | undefined): string => {
  const raw = String(value ?? "").trim();
  if (!raw || raw.toLowerCase() === "multi") return "en-US";
  const lower = raw.toLowerCase();
  if (!lower.includes("-")) {
    return TAURI_LANGUAGE_MAP[lower] ?? raw;
  }
  const [lang, region] = raw.split("-", 2);
  if (!region) return raw;
  return `${lang.toLowerCase()}-${region.toUpperCase()}`;
};
