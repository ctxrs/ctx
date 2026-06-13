import { describe, expect, it } from "vitest";
import {
  buildSessionTitlingDraft,
  buildSessionTitlingPayload,
  getSourceStepValidation,
  resolveSessionTitlingReadiness,
  resolveWorkspaceName,
} from "./WorkspaceSetupPage.logic";

describe("getSourceStepValidation", () => {
  it("allows sandbox clone with repo URL and blank source path", () => {
    const state = getSourceStepValidation({
      source: "clone",
      sourcePath: "",
      repoUrl: "https://github.com/acme/react.git",
      useSandboxStaging: true,
    });

    expect(state.needsSourcePath).toBe(false);
    expect(state.hasRepoUrl).toBe(true);
    expect(state.isComplete).toBe(true);
  });

  it("still requires repo URL for sandbox clone", () => {
    const state = getSourceStepValidation({
      source: "clone",
      sourcePath: "",
      repoUrl: "",
      useSandboxStaging: true,
    });

    expect(state.needsSourcePath).toBe(false);
    expect(state.hasRepoUrl).toBe(false);
    expect(state.isComplete).toBe(false);
  });

  it("allows sandbox new with blank source path", () => {
    const state = getSourceStepValidation({
      source: "new",
      sourcePath: "",
      repoUrl: "",
      useSandboxStaging: true,
    });

    expect(state.needsSourcePath).toBe(false);
    expect(state.isComplete).toBe(true);
  });

  it("requires source path for non-disk clone and validates clone destination shape", () => {
    const missingPath = getSourceStepValidation({
      source: "clone",
      sourcePath: "",
      repoUrl: "https://github.com/acme/react.git",
      useSandboxStaging: false,
    });
    expect(missingPath.isComplete).toBe(false);

    const invalidPath = getSourceStepValidation({
      source: "clone",
      sourcePath: "relative",
      repoUrl: "https://github.com/acme/react.git",
      useSandboxStaging: false,
    });
    expect(invalidPath.hasValidCloneDestination).toBe(false);
    expect(invalidPath.isComplete).toBe(false);

    const validPath = getSourceStepValidation({
      source: "clone",
      sourcePath: "/home/dev/projects/",
      repoUrl: "https://github.com/acme/react.git",
      useSandboxStaging: false,
    });
    expect(validPath.isComplete).toBe(true);
  });

  it("requires source path for non-disk new", () => {
    const missingPath = getSourceStepValidation({
      source: "new",
      sourcePath: "",
      repoUrl: "",
      useSandboxStaging: false,
    });
    expect(missingPath.isComplete).toBe(false);

    const withPath = getSourceStepValidation({
      source: "new",
      sourcePath: "/home/dev/new-repo",
      repoUrl: "",
      useSandboxStaging: false,
    });
    expect(withPath.isComplete).toBe(true);
  });
});

describe("resolveWorkspaceName", () => {
  it("uses friendly fallback for sandbox new", () => {
    const name = resolveWorkspaceName({
      source: "new",
      workspaceName: "",
      repoUrl: "",
      destPath: "/tmp/workspaces/staging/9a8f6f9a-7f7d-4c99-a486-1a5f6c0eff3f",
      useSandboxStaging: true,
      existingWorkspaceNames: [],
    });

    expect(name).toBe("new-workspace");
  });

  it("dedupes generated sandbox new names", () => {
    const name = resolveWorkspaceName({
      source: "new",
      workspaceName: "",
      repoUrl: "",
      destPath: "/tmp/workspaces/staging/9a8f6f9a-7f7d-4c99-a486-1a5f6c0eff3f",
      useSandboxStaging: true,
      existingWorkspaceNames: ["new-workspace", "new-workspace 2"],
    });

    expect(name).toBe("new-workspace 3");
  });

  it("preserves user-provided names without suffixing", () => {
    const name = resolveWorkspaceName({
      source: "new",
      workspaceName: "my workspace",
      repoUrl: "",
      destPath: "/tmp/workspaces/staging/9a8f6f9a-7f7d-4c99-a486-1a5f6c0eff3f",
      useSandboxStaging: true,
      existingWorkspaceNames: ["my workspace"],
    });

    expect(name).toBe("my workspace");
  });

  it("derives clone name from repo URL and dedupes generated collisions", () => {
    const name = resolveWorkspaceName({
      source: "clone",
      workspaceName: "",
      repoUrl: "https://github.com/acme/react.git",
      destPath: null,
      useSandboxStaging: true,
      existingWorkspaceNames: ["react"],
    });

    expect(name).toBe("react 2");
  });
});

describe("session titling readiness", () => {
  it("returns missing when title generation is not configured", () => {
    const readiness = resolveSessionTitlingReadiness({}, null);
    expect(readiness).toEqual({ ready: false, reason: "missing" });
  });

  it("requires remote base_url/api_key/model when mode is remote", () => {
    const incomplete = resolveSessionTitlingReadiness({
      title_generation: {
        mode: "remote",
        remote: { base_url: "https://openrouter.ai/api/v1", api_key_set: false, model: "gpt-4o-mini", use_json: true },
        local: { model_id: "ggml-org/Qwen3-1.7B-GGUF", use_json: true },
      },
    }, null);
    expect(incomplete).toEqual({ ready: false, reason: "remote_incomplete" });

    const ready = resolveSessionTitlingReadiness({
      title_generation: {
        mode: "remote",
        remote: { base_url: "https://openrouter.ai/api/v1", api_key_set: true, model: "gpt-4o-mini", use_json: true },
        local: { model_id: "ggml-org/Qwen3-1.7B-GGUF", use_json: true },
      },
    }, null);
    expect(ready).toEqual({ ready: true, reason: "remote_ready" });
  });

  it("treats local mode as configured once model is set", () => {
    const configured = resolveSessionTitlingReadiness({
      title_generation: {
        mode: "local",
        remote: { base_url: "", api_key_set: false, model: "", use_json: true },
        local: { model_id: "ggml-org/Qwen3-1.7B-GGUF", use_json: true },
      },
    }, { ready: false } as never);
    expect(configured).toEqual({ ready: true, reason: "local_ready" });

    const missingModel = resolveSessionTitlingReadiness({
      title_generation: {
        mode: "local",
        remote: { base_url: "", api_key_set: false, model: "", use_json: true },
        local: { model_id: "", use_json: true },
      },
    }, { ready: true } as never);
    expect(missingModel).toEqual({ ready: false, reason: "local_missing_model" });
  });
});

describe("session titling payload", () => {
  it("hydrates draft defaults and existing values", () => {
    const draft = buildSessionTitlingDraft({
      title_generation: {
        mode: "remote",
        remote: { base_url: "https://api.example", api_key_set: true, model: "gpt-x", use_json: false },
        local: { model_id: "local-model", use_json: false },
      },
    });

    expect(draft.mode).toBe("remote");
    expect(draft.remote.baseUrl).toBe("https://api.example");
    expect(draft.remote.apiKey).toBe("");
    expect(draft.remote.model).toBe("gpt-x");
    expect(draft.remote.useJson).toBe(false);
    expect(draft.local.modelId).toBe("local-model");
    expect(draft.local.useJson).toBe(false);
  });

  it("builds payload with explicit mode and complete schema", () => {
    const payload = buildSessionTitlingPayload({
      mode: "local",
      draft: {
        mode: "local",
        remote: {
          baseUrl: "https://api.example",
          apiKey: "sk-live",
          model: "gpt-y",
          useJson: true,
        },
        local: {
          modelId: "ggml-org/Qwen3-1.7B-GGUF",
          useJson: false,
        },
      },
      existing: null,
    });

    expect(payload).toEqual({
      mode: "local",
      remote: {
        base_url: "https://api.example",
        api_key: "sk-live",
        model: "gpt-y",
        use_json: true,
      },
      local: {
        model_id: "ggml-org/Qwen3-1.7B-GGUF",
        use_json: false,
      },
    });
  });

  it("omits unchanged remote API keys so stored secrets are preserved", () => {
    const payload = buildSessionTitlingPayload({
      mode: "remote",
      draft: {
        mode: "remote",
        remote: {
          baseUrl: "https://api.example",
          apiKey: "",
          model: "gpt-existing",
          useJson: true,
        },
        local: {
          modelId: "ggml-org/Qwen3-1.7B-GGUF",
          useJson: true,
        },
      },
      existing: {
        mode: "remote",
        remote: {
          base_url: "https://api.example",
          api_key_set: true,
          model: "gpt-existing",
          use_json: true,
        },
        local: { model_id: "ggml-org/Qwen3-1.7B-GGUF", use_json: true },
      },
    });

    expect(payload).toEqual({
      mode: "remote",
      remote: {
        base_url: "https://api.example",
        model: "gpt-existing",
        use_json: true,
      },
      local: {
        model_id: "ggml-org/Qwen3-1.7B-GGUF",
        use_json: true,
      },
    });
  });
});
