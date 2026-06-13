"use strict";

const normalizeTarget = (target) => {
  const value = String(target || "host").trim();
  return value === "container" ? "container" : "host";
};

const providerStatusPath = (providerId, target = "host") => {
  const id = encodeURIComponent(String(providerId || "").trim());
  const resolvedTarget = normalizeTarget(target);
  return `/api/providers/${id}?target=${resolvedTarget}`;
};

module.exports = {
  normalizeTarget,
  providerStatusPath,
};
