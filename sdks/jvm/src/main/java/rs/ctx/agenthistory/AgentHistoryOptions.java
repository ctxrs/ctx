package rs.ctx.agenthistory;

import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;

/** Typed builders for agent-history-v1 operations. */
public final class AgentHistoryOptions {
    private AgentHistoryOptions() {}

    public static Init init() {
        return new Init();
    }

    public static ImportHistory importHistory() {
        return new ImportHistory();
    }

    public static Search search() {
        return new Search();
    }

    public static ShowEvent showEvent() {
        return new ShowEvent();
    }

    public static ShowSession showSession() {
        return new ShowSession();
    }

    public static LocateSession locateSession() {
        return new LocateSession();
    }

    public static final class Init {
        private boolean catalogOnly;
        private String progress;

        public boolean catalogOnly() {
            return catalogOnly;
        }

        public String progress() {
            return progress;
        }

        public Init catalogOnly(boolean catalogOnly) {
            this.catalogOnly = catalogOnly;
            return this;
        }

        public Init progress(String progress) {
            this.progress = progress;
            return this;
        }
    }

    public static final class ImportHistory {
        private boolean all;
        private String provider;
        private String path;
        private boolean resume;
        private String progress;

        public boolean all() {
            return all;
        }

        public String provider() {
            return provider;
        }

        public String path() {
            return path;
        }

        public boolean resume() {
            return resume;
        }

        public String progress() {
            return progress;
        }

        public ImportHistory all(boolean all) {
            this.all = all;
            return this;
        }

        public ImportHistory provider(String provider) {
            this.provider = provider;
            return this;
        }

        public ImportHistory path(String path) {
            this.path = path;
            return this;
        }

        public ImportHistory path(Path path) {
            this.path = path == null ? null : path.toString();
            return this;
        }

        public ImportHistory resume(boolean resume) {
            this.resume = resume;
            return this;
        }

        public ImportHistory progress(String progress) {
            this.progress = progress;
            return this;
        }
    }

    public static final class Search {
        private SearchQuery query;
        private Integer limit;
        private String backend;
        private String provider;
        private String workspace;
        private String since;
        private boolean primaryOnly;
        private boolean includeSubagents;
        private String eventType;
        private String file;
        private String session;
        private boolean events;
        private String refresh;
        private boolean includeCurrentSession;

        public SearchQuery query() {
            return query;
        }

        public Integer limit() {
            return limit;
        }

        public String backend() {
            return backend;
        }


        public String provider() {
            return provider;
        }

        public String workspace() {
            return workspace;
        }

        public String since() {
            return since;
        }

        public boolean primaryOnly() {
            return primaryOnly;
        }

        public boolean includeSubagents() {
            return includeSubagents;
        }

        public String eventType() {
            return eventType;
        }

        public String file() {
            return file;
        }

        public String session() {
            return session;
        }

        public boolean events() {
            return events;
        }

        public String refresh() {
            return refresh;
        }

        public boolean includeCurrentSession() {
            return includeCurrentSession;
        }

        public Search query(SearchQuery query) {
            this.query = query;
            return this;
        }

        public Search limit(Integer limit) {
            this.limit = limit;
            return this;
        }

        public Search backend(String backend) {
            this.backend = backend;
            return this;
        }


        public Search provider(String provider) {
            this.provider = provider;
            return this;
        }

        public Search workspace(String workspace) {
            this.workspace = workspace;
            return this;
        }

        public Search since(String since) {
            this.since = since;
            return this;
        }

        public Search primaryOnly(boolean primaryOnly) {
            this.primaryOnly = primaryOnly;
            return this;
        }

        public Search includeSubagents(boolean includeSubagents) {
            this.includeSubagents = includeSubagents;
            return this;
        }

        public Search eventType(String eventType) {
            this.eventType = eventType;
            return this;
        }

        public Search file(String file) {
            this.file = file;
            return this;
        }

        public Search file(Path file) {
            this.file = file == null ? null : file.toString();
            return this;
        }

        public Search session(String session) {
            this.session = session;
            return this;
        }

        public Search events(boolean events) {
            this.events = events;
            return this;
        }

        public Search refresh(String refresh) {
            this.refresh = refresh;
            return this;
        }

        public Search includeCurrentSession(boolean includeCurrentSession) {
            this.includeCurrentSession = includeCurrentSession;
            return this;
        }
    }

    public static final class ShowEvent {
        private Integer before;
        private Integer after;
        private Integer window;

        public Integer before() {
            return before;
        }

        public Integer after() {
            return after;
        }

        public Integer window() {
            return window;
        }

        public ShowEvent before(Integer before) {
            this.before = before;
            return this;
        }

        public ShowEvent after(Integer after) {
            this.after = after;
            return this;
        }

        public ShowEvent window(Integer window) {
            this.window = window;
            return this;
        }
    }

    public static final class ShowSession {
        private String id;
        private String provider;
        private String providerSessionId;
        private String mode;

        public String id() {
            return id;
        }

        public String provider() {
            return provider;
        }

        public String providerSessionId() {
            return providerSessionId;
        }

        public String mode() {
            return mode;
        }

        public ShowSession id(String id) {
            this.id = id;
            return this;
        }

        public ShowSession provider(String provider) {
            this.provider = provider;
            return this;
        }

        public ShowSession providerSessionId(String providerSessionId) {
            this.providerSessionId = providerSessionId;
            return this;
        }

        public ShowSession mode(String mode) {
            this.mode = mode;
            return this;
        }
    }

    public static final class LocateSession {
        private String id;
        private String provider;
        private String providerSessionId;

        public String id() {
            return id;
        }

        public String provider() {
            return provider;
        }

        public String providerSessionId() {
            return providerSessionId;
        }

        public LocateSession id(String id) {
            this.id = id;
            return this;
        }

        public LocateSession provider(String provider) {
            this.provider = provider;
            return this;
        }

        public LocateSession providerSessionId(String providerSessionId) {
            this.providerSessionId = providerSessionId;
            return this;
        }
    }
}
