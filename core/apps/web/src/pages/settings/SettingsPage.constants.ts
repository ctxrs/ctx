import type { DesktopEditorSettings, DesktopUpdateChannelSettings } from "../../utils/desktop";
import type { SectionId } from "./SettingsPage.types";

export const AGENT_PROMPT_DEFAULT = "You are working inside ctx, an agent development environment. Use ctx MCP tools to attach photos/videos as artifacts, start persistent web sessions (Playwright REPL/scripts), and run sub-agents for research or well-scoped implementations. Check `.ctx/attachments/refs/` and `.ctx/attachments/docs/` for extra reference repos and docs." as const;
export const SUBAGENT_PROMPT_DEFAULT = "You are a subagent. The user messaging you is the primary agent who will provide your instructions." as const;

export const MODEL_OPTIONS: Array<{ value: string; label: string }> = [
  { value: "auto", label: "Default (Deepgram Nova-3)" },
  { value: "deepgram/flux-general", label: "Deepgram Flux" },
  { value: "deepgram/nova-3", label: "Deepgram Nova-3" },
  { value: "deepgram/nova-3-medical", label: "Deepgram Nova-3 Medical" },
  { value: "deepgram/nova-2", label: "Deepgram Nova-2" },
  { value: "deepgram/nova-2-medical", label: "Deepgram Nova-2 Medical" },
  { value: "deepgram/nova-2-conversationalai", label: "Deepgram Nova-2 Conversational AI" },
  { value: "deepgram/nova-2-phonecall", label: "Deepgram Nova-2 Phonecall" },
  { value: "assemblyai/universal-streaming", label: "AssemblyAI Universal-Streaming" },
  { value: "assemblyai/universal-streaming-multilingual", label: "AssemblyAI Universal-Streaming Multilingual" },
  { value: "cartesia/ink-whisper", label: "Cartesia Ink Whisper" },
  { value: "elevenlabs/scribe_v2_realtime", label: "ElevenLabs Scribe V2 Realtime" },
];

export const EDITOR_OPTIONS: Array<{ value: DesktopEditorSettings["target"]; label: string }> = [
  { value: "system", label: "System default" },
  { value: "vscode", label: "Visual Studio Code" },
  { value: "vscode_insiders", label: "Visual Studio Code Insiders" },
  { value: "cursor", label: "Cursor" },
  { value: "windsurf", label: "Windsurf" },
  { value: "antigravity", label: "Google Antigravity" },
  { value: "idea", label: "IntelliJ IDEA" },
  { value: "pycharm", label: "PyCharm" },
  { value: "xcode", label: "Xcode" },
  { value: "android_studio", label: "Android Studio" },
];

export const UPDATE_CHANNEL_OPTIONS: Array<{ value: DesktopUpdateChannelSettings["channel"]; label: string }> = [
  { value: "stable", label: "Stable" },
  { value: "canary", label: "Canary" },
];

export const SECTIONS: Array<{
  id: SectionId;
  label: string;
  group?: "main" | "advanced";
  navHidden?: boolean;
}> = [
  { id: "general", label: "General", group: "main" },
  { id: "notifications", label: "Notifications", group: "main" },
  { id: "agent_harnesses", label: "Harness Authentication", group: "main" },
  { id: "harness_subscriptions", label: "Harness Subscriptions", group: "main", navHidden: true },
  { id: "models_routing", label: "Models & Routing", group: "main", navHidden: true },
  { id: "container_network", label: "Sandbox & Networking", group: "main" },
  { id: "worktree_bootstrap", label: "Worktree Lifecycle", group: "main" },
  { id: "agent_system_prompt", label: "Agent System Prompt", group: "main" },
  { id: "workspace_attachments", label: "Workspace Attachments", group: "main" },
  { id: "merge_queue", label: "Merge Queue", group: "main" },
  { id: "context_pack", label: "ctx pack", group: "main", navHidden: true },
  { id: "resource_utilization", label: "Resource Utilization", group: "main", navHidden: true },
  { id: "analytics", label: "Analytics", group: "main" },
  { id: "dictation", label: "Dictation", group: "advanced", navHidden: true },
  { id: "title_generation", label: "Title Generation", group: "advanced" },
  { id: "usage_analytics", label: "Usage Analytics", group: "advanced", navHidden: true },
  { id: "dev_tools", label: "Dev Tools", group: "advanced" },
];
