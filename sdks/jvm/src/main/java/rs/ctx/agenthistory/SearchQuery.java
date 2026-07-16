package rs.ctx.agenthistory;

import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Collections;
import java.util.HashSet;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;

/** Canonical structured ctx-search-v1 query. */
public final class SearchQuery {
    public static final String VERSION = "ctx-search-v1";
    public static final int MAX_CLAUSES = 32;
    public static final int MAX_CLAUSE_BYTES = 1_024;
    public static final int MAX_TOTAL_CLAUSE_BYTES = 8_192;
    public static final int MAX_JSON_BYTES = 64 * 1_024;
    public static final int MIN_LITERAL_BYTES = 3;
    public static final int MAX_LITERAL_BYTES = 256;
    public static final int MAX_ANALYZED_TOKENS_PER_CLAUSE = 32;

    private final String version;
    private final List<SearchClause> any;
    private final List<SearchClause> must;
    private final List<SearchClause> mustNot;

    private SearchQuery(Builder builder) {
        this.version = builder.version;
        this.any = canonical(builder.any);
        this.must = canonical(builder.must);
        this.mustNot = canonical(builder.mustNot);
        validate();
    }

    public static Builder builder() { return new Builder(); }
    public static SearchQuery all(String value) { return builder().any(SearchClause.all(value)).build(); }
    public String version() { return version; }
    public List<SearchClause> any() { return any; }
    public List<SearchClause> must() { return must; }
    public List<SearchClause> mustNot() { return mustNot; }

    public SearchQuery validate() {
        if (!VERSION.equals(version)) throw invalid("search query version must be ctx-search-v1");
        if (any.size() + must.size() == 0) throw invalid("search query needs a positive any or must clause");
        int count = 0;
        int totalBytes = 0;
        int semanticCount = 0;
        for (Placement placement : placements()) {
            for (SearchClause clause : placement.clauses) {
                if (clause == null) throw invalid("search clause cannot be null");
                if (!"any".equals(placement.name) && "semantic".equals(clause.matcher())) {
                    throw invalid("semantic clauses are allowed only in any");
                }
                if ("semantic".equals(clause.matcher()) && ++semanticCount > 1) {
                    throw invalid("search query allows at most one semantic clause in any");
                }
                if (clause.value() == null) throw invalid("search clause value must be a string");
                int bytes = clause.value().getBytes(StandardCharsets.UTF_8).length;
                if (bytes == 0) throw invalid("search clause cannot be empty");
                if (bytes > MAX_CLAUSE_BYTES) throw invalid("search clause exceeds the 1024-byte limit");
                if ("literal".equals(clause.matcher()) && (bytes < MIN_LITERAL_BYTES || bytes > MAX_LITERAL_BYTES)) {
                    throw invalid("literal search clause must be between 3 and 256 bytes");
                }
                int tokens = analyzedTokenCount(clause.value());
                if (tokens == 0) throw invalid("search clause has no searchable tokens");
                if (tokens > MAX_ANALYZED_TOKENS_PER_CLAUSE) {
                    throw invalid("search clause exceeds the 32 analyzed-token limit");
                }
                count++;
                totalBytes += bytes;
            }
        }
        if (count > MAX_CLAUSES) throw invalid("search query exceeds the 32-clause limit");
        if (totalBytes > MAX_TOTAL_CLAUSE_BYTES) throw invalid("search query exceeds the 8192-byte clause limit");
        return this;
    }

    public Map<String, Object> asMap() {
        Map<String, Object> out = new LinkedHashMap<>();
        out.put("version", version);
        add(out, "any", any);
        add(out, "must", must);
        add(out, "must_not", mustNot);
        return AgentHistoryValue.copyObject(out);
    }

    public String toJson() {
        String json = Json.stringify(asMap());
        if (json.getBytes(StandardCharsets.UTF_8).length > MAX_JSON_BYTES) {
            throw invalid("search query JSON exceeds the 65536-byte limit");
        }
        return json;
    }

    static SearchQuery fromMap(Map<String, Object> raw) {
        for (String key : raw.keySet()) {
            if (!"version".equals(key) && !"any".equals(key) && !"must".equals(key) && !"must_not".equals(key)) {
                throw invalid("search query contains unknown field '" + key + "'");
            }
        }
        Builder builder = builder().version(AgentHistoryValue.string(raw.get("version")));
        read(builder, raw, "any");
        read(builder, raw, "must");
        read(builder, raw, "must_not");
        return builder.build();
    }

    private static void read(Builder builder, Map<String, Object> raw, String placement) {
        if (!raw.containsKey(placement)) return;
        Object value = raw.get(placement);
        if (!(value instanceof List<?>)) throw invalid("search query " + placement + " must be an array");
        for (Object item : (List<?>) value) {
            Map<String, Object> clause = AgentHistoryValue.objectOrNull(item);
            if (clause == null) throw invalid("search clause must be an object");
            builder.add(placement, SearchClause.fromMap(clause, placement));
        }
    }

    private List<Placement> placements() {
        List<Placement> values = new ArrayList<>();
        values.add(new Placement("any", any)); values.add(new Placement("must", must));
        values.add(new Placement("must_not", mustNot)); return values;
    }

    private static List<SearchClause> canonical(List<SearchClause> clauses) {
        List<SearchClause> out = new ArrayList<>();
        Set<String> seen = new HashSet<>();
        for (SearchClause clause : clauses) {
            if (clause == null || clause.value() == null) {
                out.add(clause);
                continue;
            }
            String value = canonicalValue(clause.matcher(), clause.value());
            String identity = clause.matcher() + "\0" + value;
            if (seen.add(identity)) out.add(clause.withValue(value));
        }
        return Collections.unmodifiableList(out);
    }

    private static String canonicalValue(String matcher, String value) {
        if ("literal".equals(matcher)) return trimWhitespace(value);
        StringBuilder out = new StringBuilder(value.length());
        boolean pendingSpace = false;
        for (int offset = 0; offset < value.length();) {
            int current = value.codePointAt(offset);
            offset += Character.charCount(current);
            if (isSearchWhitespace(current)) {
                if (out.length() > 0) pendingSpace = true;
            } else {
                if (pendingSpace) out.append(' ');
                out.appendCodePoint(current);
                pendingSpace = false;
            }
        }
        return out.toString();
    }

    private static String trimWhitespace(String value) {
        int start = 0;
        int end = value.length();
        while (start < end) {
            int current = value.codePointAt(start);
            if (!isSearchWhitespace(current)) break;
            start += Character.charCount(current);
        }
        while (start < end) {
            int current = value.codePointBefore(end);
            if (!isSearchWhitespace(current)) break;
            end -= Character.charCount(current);
        }
        return value.substring(start, end);
    }

    private static int analyzedTokenCount(String value) {
        int count = 0;
        boolean inToken = false;
        for (int offset = 0; offset < value.length();) {
            int current = value.codePointAt(offset);
            offset += Character.charCount(current);
            boolean continues = isSearchAlphanumeric(current)
                    || (inToken && isSearchContinuationMark(current));
            if (continues) {
                if (!inToken) count++;
                inToken = true;
            } else {
                inToken = false;
            }
        }
        return count;
    }

    private static boolean isSearchAlphanumeric(int current) {
        int type = Character.getType(current);
        return Character.isLetter(current)
                || type == Character.DECIMAL_DIGIT_NUMBER
                || type == Character.LETTER_NUMBER
                || type == Character.OTHER_NUMBER;
    }

    private static boolean isSearchWhitespace(int current) {
        return current >= 0x0009 && current <= 0x000d
                || current == 0x0020
                || current == 0x0085
                || current == 0x00a0
                || current == 0x1680
                || current >= 0x2000 && current <= 0x200a
                || current == 0x2028
                || current == 0x2029
                || current == 0x202f
                || current == 0x205f
                || current == 0x3000;
    }

    private static boolean isSearchContinuationMark(int current) {
        return current >= 0x0300 && current <= 0x036f
                || current >= 0x1ab0 && current <= 0x1aff
                || current >= 0x1dc0 && current <= 0x1dff
                || current >= 0x20d0 && current <= 0x20ff
                || current >= 0xfe20 && current <= 0xfe2f
                || current == 0x200c
                || current == 0x200d;
    }

    private static void add(Map<String, Object> out, String name, List<SearchClause> clauses) {
        if (clauses.isEmpty()) return;
        List<Object> values = new ArrayList<>();
        for (SearchClause clause : clauses) values.add(clause.asMap());
        out.put(name, values);
    }

    private static CtxAgentHistoryException.Validation invalid(String message) {
        return new CtxAgentHistoryException.Validation(message);
    }

    private static final class Placement {
        private final String name; private final List<SearchClause> clauses;
        private Placement(String name, List<SearchClause> clauses) { this.name = name; this.clauses = clauses; }
    }

    public static final class Builder {
        private String version = VERSION;
        private final List<SearchClause> any = new ArrayList<>();
        private final List<SearchClause> must = new ArrayList<>();
        private final List<SearchClause> mustNot = new ArrayList<>();
        public Builder version(String value) { version = value; return this; }
        public Builder any(SearchClause value) { any.add(value); return this; }
        public Builder must(SearchClause value) { must.add(value); return this; }
        public Builder mustNot(SearchClause value) { mustNot.add(value); return this; }
        private Builder add(String placement, SearchClause value) {
            if ("any".equals(placement)) return any(value);
            if ("must".equals(placement)) return must(value);
            return mustNot(value);
        }
        public SearchQuery build() { return new SearchQuery(this); }
    }
}
