package rs.ctx.agenthistory;

import java.util.ArrayList;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Objects;

public class AgentHistoryClient {
    private final AgentHistoryTransport transport;

    protected AgentHistoryClient(AgentHistoryTransport transport) {
        this.transport = transport;
    }

    public static AgentHistoryClient local() {
        return local(LocalCliConfig.builder().build());
    }

    public static AgentHistoryClient local(LocalCliConfig config) {
        return new AgentHistoryClient(new LocalCliAdapter(config));
    }

    public static HostedAgentHistoryClient hosted(HostedConfig config) {
        return new HostedAgentHistoryClient(config);
    }

    public static AgentHistoryClient withTransport(AgentHistoryTransport transport) {
        return new AgentHistoryClient(Objects.requireNonNull(transport, "transport"));
    }

    public StatusResponse status() {
        return new StatusResponse(executeEnvelope("status", list("status", "--json")));
    }

    public InitResponse init() {
        return init(AgentHistoryOptions.init());
    }

    public InitResponse init(AgentHistoryOptions.Init options) {
        AgentHistoryOptions.Init safe = options == null ? AgentHistoryOptions.init() : options;
        List<String> args = mutable("setup", "--json");
        if (safe.progress() != null) {
            args.add("--progress");
            args.add(safe.progress());
        } else {
            args.add("--progress");
            args.add("none");
        }
        if (safe.catalogOnly()) {
            args.add("--catalog-only");
        }
        return new InitResponse(executeEnvelope("init", args));
    }

    public SourcesResponse sources() {
        return new SourcesResponse(executeEnvelope("sources", list("sources", "--json")));
    }

    public ImportResponse importHistory() {
        return importHistory(AgentHistoryOptions.importHistory());
    }

    public ImportResponse importHistory(AgentHistoryOptions.ImportHistory options) {
        return new ImportResponse(executeEnvelope("import", importArgs(options)));
    }

    public ImportResponse sync() {
        return sync(AgentHistoryOptions.importHistory());
    }

    public ImportResponse sync(AgentHistoryOptions.ImportHistory options) {
        return new ImportResponse(executeEnvelope("sync", importArgs(options)));
    }

    public SearchResponse search() {
        return search(AgentHistoryOptions.search());
    }

    public SearchResponse search(AgentHistoryOptions.Search options) {
        AgentHistoryOptions.Search safe = options == null ? AgentHistoryOptions.search() : options;
        requireSearchIntent(safe);
        List<String> args = new ArrayList<>();
        args.add("search");
        if (safe.query() != null) {
            args.add("--query-json");
            args.add(safe.query().toJson());
        }
        args.add("--json");
        if (safe.limit() != null) {
            args.add("--limit");
            args.add(String.valueOf(safe.limit()));
        }
        add(args, "--backend", safe.backend());
        add(args, "--provider", safe.provider());
        add(args, "--workspace", safe.workspace());
        add(args, "--since", safe.since());
        add(args, "--event-type", safe.eventType());
        add(args, "--file", safe.file());
        add(args, "--session", safe.session());
        add(args, "--refresh", safe.refresh());
        if (safe.primaryOnly()) args.add("--primary-only");
        if (safe.includeSubagents()) args.add("--include-subagents");
        if (safe.events()) args.add("--events");
        if (safe.includeCurrentSession()) args.add("--include-current-session");
        return new SearchResponse(executeEnvelope("search", args));
    }

    private static void requireSearchIntent(AgentHistoryOptions.Search options) {
        if (options.query() != null) {
            options.query().validate();
            return;
        }
        if (hasText(options.file())) {
            return;
        }
        throw new CtxAgentHistoryException.Validation("search requires a ctx-search-v1 query or file option");
    }

    public ShowEventResponse showEvent(String id, AgentHistoryOptions.ShowEvent options) {
        if (id == null || id.isEmpty()) {
            throw new CtxAgentHistoryException.Validation("event id is required");
        }
        AgentHistoryOptions.ShowEvent safe = options == null ? AgentHistoryOptions.showEvent() : options;
        List<String> args = mutable("show", "event", id, "--format", "json");
        addInt(args, "--before", safe.before());
        addInt(args, "--after", safe.after());
        addInt(args, "--window", safe.window());
        return new ShowEventResponse(executeEnvelope("showEvent", args));
    }

    public ShowEventResponse showEvent(String id) {
        return showEvent(id, AgentHistoryOptions.showEvent());
    }

    public ShowSessionResponse showSession(String id, AgentHistoryOptions.ShowSession options) {
        AgentHistoryOptions.ShowSession safe = options == null ? AgentHistoryOptions.showSession() : options;
        String sessionId = id == null ? safe.id() : id;
        List<String> args = mutable("show", "session");
        if (sessionId != null && !sessionId.isEmpty()) {
            args.add(sessionId);
        } else {
            add(args, "--provider", safe.provider());
            add(args, "--provider-session", safe.providerSessionId());
        }
        add(args, "--mode", safe.mode());
        args.add("--format");
        args.add("json");
        return new ShowSessionResponse(executeEnvelope("showSession", args));
    }

    public ShowSessionResponse showSession(String id) {
        return showSession(id, AgentHistoryOptions.showSession());
    }

    public LocateEventResponse locateEvent(String id) {
        if (id == null || id.isEmpty()) {
            throw new CtxAgentHistoryException.Validation("event id is required");
        }
        return new LocateEventResponse(executeEnvelope("locateEvent", list("locate", "event", id, "--format", "json")));
    }

    public LocateSessionResponse locateSession(String id) {
        return locateSession(AgentHistoryOptions.locateSession().id(id));
    }

    public LocateSessionResponse locateSession(AgentHistoryOptions.LocateSession options) {
        AgentHistoryOptions.LocateSession safe = options == null ? AgentHistoryOptions.locateSession() : options;
        List<String> args = mutable("locate", "session");
        if (safe.id() != null && !safe.id().isEmpty()) {
            args.add(safe.id());
        } else {
            add(args, "--provider", safe.provider());
            add(args, "--provider-session", safe.providerSessionId());
        }
        args.add("--format");
        args.add("json");
        return new LocateSessionResponse(executeEnvelope("locateSession", args));
    }

    public VersionInfo version() {
        return new VersionInfo(transport.name(), transport.ctxVersion());
    }

    public Map<String, Object> versioning() {
        return version().asMap();
    }

    protected AgentHistoryEnvelope execute(String operation, List<String> args) {
        return new AgentHistoryEnvelope(executeEnvelope(operation, args));
    }

    protected Map<String, Object> executeEnvelope(String operation, List<String> args) {
        String stdout = transport.execute(new AgentHistoryOperation(operation, args));
        Map<String, Object> raw;
        try {
            raw = Json.parseObject(stdout);
        } catch (RuntimeException error) {
            Map<String, Object> details = new LinkedHashMap<>();
            details.put("operation", operation);
            details.put("stdout", stdout);
            throw new CtxAgentHistoryException.Protocol("ctx command returned invalid JSON", details, error);
        }
        return AgentHistoryEnvelope.normalize(operation, backend(), raw);
    }

    protected Backend backend() {
        if (transport.name().startsWith("hosted")) {
            return new Backend("hosted", null, null);
        }
        String dataRoot = null;
        if (transport instanceof LocalCliAdapter) {
            dataRoot = ((LocalCliAdapter) transport).config().dataRoot();
        }
        return new Backend("local", dataRoot, null);
    }

    private static List<String> importArgs(AgentHistoryOptions.ImportHistory options) {
        AgentHistoryOptions.ImportHistory safe = options == null ? AgentHistoryOptions.importHistory() : options;
        List<String> args = mutable("import", "--json");
        if (safe.progress() != null) {
            args.add("--progress");
            args.add(safe.progress());
        } else {
            args.add("--progress");
            args.add("none");
        }
        if (safe.all()) args.add("--all");
        add(args, "--provider", safe.provider());
        add(args, "--path", safe.path());
        if (safe.resume()) args.add("--resume");
        return args;
    }

    private static void add(List<String> args, String flag, String value) {
        if (value != null && !value.isEmpty()) {
            args.add(flag);
            args.add(value);
        }
    }

    private static boolean hasText(String value) {
        return value != null && !value.trim().isEmpty();
    }

    private static void addInt(List<String> args, String flag, Integer value) {
        if (value != null) {
            args.add(flag);
            args.add(String.valueOf(value));
        }
    }

    private static List<String> list(String... values) {
        List<String> out = mutable(values);
        return Collections.unmodifiableList(out);
    }

    private static List<String> mutable(String... values) {
        List<String> out = new ArrayList<>();
        Collections.addAll(out, values);
        return out;
    }
}
