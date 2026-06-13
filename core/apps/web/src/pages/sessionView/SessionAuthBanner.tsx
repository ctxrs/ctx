import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "../../components/ui/select";

type AuthMethod = {
  id: string;
  name: string;
};

type SessionAuthBannerProps = {
  visible: boolean;
  status: "required" | "failed" | string;
  provider?: string | null;
  message?: string | null;
  methods: AuthMethod[];
  authMethodId: string;
  onAuthMethodChange: (value: string) => void;
  authBusy: boolean;
  authError: string | null;
  onAuthenticate: () => void | Promise<void>;
};

export function SessionAuthBanner({
  visible,
  status,
  provider,
  message,
  methods,
  authMethodId,
  onAuthMethodChange,
  authBusy,
  authError,
  onAuthenticate,
}: SessionAuthBannerProps) {
  if (!visible) return null;

  return (
    <div className="banner">
      <div className="row" style={{ justifyContent: "space-between" }}>
        <strong>Authentication required</strong>
        {provider ? <span className="muted">{provider}</span> : null}
      </div>
      <div className="muted">
        {message ?? "This provider requires authentication before it can run."}
      </div>
      {methods.length > 0 ? (
        <div className="row" style={{ flexWrap: "wrap" }}>
          {methods.length > 1 ? (
            <label>
              Method
              <Select value={authMethodId} onValueChange={onAuthMethodChange}>
                <SelectTrigger>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {methods.map((method) => (
                    <SelectItem key={method.id} value={method.id}>
                      {method.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </label>
          ) : null}
          <button type="button" disabled={authBusy || !authMethodId} onClick={() => void onAuthenticate()}>
            {authBusy ? "Authenticating..." : "Authenticate"}
          </button>
        </div>
      ) : (
        <div className="muted">No authentication methods were advertised by the provider.</div>
      )}
      {(authError || status === "failed") ? (
        <div className="muted">
          {authError ?? "Authentication attempt failed. Check provider logs and try again."}
        </div>
      ) : null}
    </div>
  );
}
