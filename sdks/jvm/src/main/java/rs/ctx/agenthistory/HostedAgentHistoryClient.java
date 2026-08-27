package rs.ctx.agenthistory;

import java.util.LinkedHashMap;
import java.util.Map;

/**
 * Explicit hosted placeholder. It never performs network calls.
 *
 * @deprecated Hosted SDK placeholders are deprecated and will be removed in the next breaking SDK
 *     revision; hosted operations remain unsupported.
 */
@Deprecated(forRemoval = true)
@SuppressWarnings("removal")
public final class HostedAgentHistoryClient extends AgentHistoryClient {
    private final HostedConfig config;

    public HostedAgentHistoryClient(HostedConfig config) {
        super(new HostedTransport(config));
        this.config = config == null ? HostedConfig.builder().build() : config;
    }

    public HostedConfig config() {
        return config;
    }

    @Override
    protected Backend backend() {
        return new Backend("hosted", null, config.baseUrl());
    }

    private static final class HostedTransport implements AgentHistoryTransport {
        private final HostedConfig config;

        HostedTransport(HostedConfig config) {
            this.config = config == null ? HostedConfig.builder().build() : config;
        }

        @Override
        public String name() {
            return "hosted-placeholder";
        }

        @Override
        public String execute(AgentHistoryOperation operation) {
            Map<String, Object> details = new LinkedHashMap<>();
            details.put("backend", "hosted");
            details.put("baseUrl", config.baseUrl());
            details.put("operation", operation.name());
            throw new CtxAgentHistoryException.Unsupported(
                    "hosted ctx agent history backend is not available in this in-repo SDK",
                    details);
        }
    }
}
