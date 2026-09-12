package rs.ctx.agenthistory;

import java.util.LinkedHashMap;
import java.util.Map;

/** Version metadata for the experimental JVM SDK and selected transport. */
public final class VersionInfo {
    public static final String CONTRACT_VERSION = "agent-history-v2";
    public static final int SCHEMA_VERSION = 1;
    public static final String SDK_VERSION = "0.1.0-experimental";

    private final String sdkVersion;
    private final String contractVersion;
    private final int schemaVersion;
    private final String adapter;
    private final String ctxVersion;

    public VersionInfo(String adapter, String ctxVersion) {
        this(SDK_VERSION, CONTRACT_VERSION, SCHEMA_VERSION, adapter, ctxVersion);
    }

    public VersionInfo(
            String sdkVersion,
            String contractVersion,
            int schemaVersion,
            String adapter,
            String ctxVersion) {
        this.sdkVersion = sdkVersion;
        this.contractVersion = contractVersion;
        this.schemaVersion = schemaVersion;
        this.adapter = adapter;
        this.ctxVersion = ctxVersion;
    }

    public String sdkVersion() {
        return sdkVersion;
    }

    public String contractVersion() {
        return contractVersion;
    }

    public int schemaVersion() {
        return schemaVersion;
    }

    public String adapter() {
        return adapter;
    }

    public String ctxVersion() {
        return ctxVersion;
    }

    public Map<String, Object> asMap() {
        Map<String, Object> out = new LinkedHashMap<>();
        out.put("contractVersion", contractVersion);
        out.put("schemaVersion", schemaVersion);
        out.put("sdkVersion", sdkVersion);
        out.put("adapter", adapter);
        out.put("ctxVersion", ctxVersion);
        return out;
    }
}
