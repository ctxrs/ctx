import { SECTIONS } from "./SettingsPage.constants";

describe("Settings sections", () => {
  it("keeps notifications visible in settings navigation", () => {
    const notifications = SECTIONS.find((section) => section.id === "notifications");

    expect(notifications).toBeDefined();
    expect(notifications?.navHidden).not.toBe(true);
  });

  it("keeps dictation hidden from the sidebar navigation", () => {
    const dictation = SECTIONS.find((section) => section.id === "dictation");

    expect(dictation).toBeDefined();
    expect(dictation?.navHidden).toBe(true);
  });

  it("keeps sandbox and networking visible in settings navigation without a separate sandboxing entry", () => {
    const sandboxAndNetworking = SECTIONS.find((section) => section.id === "container_network");
    const sectionIds = SECTIONS.map((section) => String(section.id));

    expect(sandboxAndNetworking).toBeDefined();
    expect(sandboxAndNetworking?.label).toBe("Sandbox & Networking");
    expect(sandboxAndNetworking?.navHidden).not.toBe(true);
    expect(sectionIds).not.toContain("sandboxing");
  });

  it("omits hosted account and managed-service settings from the public export", () => {
    const sectionIds = SECTIONS.map((section) => String(section.id));

    expect(sectionIds).not.toContain("billing");
    expect(sectionIds).not.toContain("team_enterprise");
    expect(sectionIds).not.toContain("mobile_access");
  });
});
