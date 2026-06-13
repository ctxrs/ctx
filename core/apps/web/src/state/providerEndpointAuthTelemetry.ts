import {
  trackProviderAuthCompleted,
  trackProviderAuthFailed,
  trackProviderAuthStarted,
  type ProviderAuthFailureKind,
} from "../utils/analytics";

export const trackEndpointAuthStarted = (providerId: string): void => {
  trackProviderAuthStarted({
    providerId,
    authMethod: "endpoint",
  });
};

export const trackEndpointAuthCompleted = (providerId: string): void => {
  trackProviderAuthCompleted({
    providerId,
    authMethod: "endpoint",
  });
};

export const trackEndpointAuthFailed = (
  providerId: string,
  failureKind: ProviderAuthFailureKind,
): void => {
  trackProviderAuthFailed({
    providerId,
    authMethod: "endpoint",
    failureKind,
  });
};
