package rs.ctx.agenthistory;

import java.util.ArrayList;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.function.Function;

final class AgentHistoryValue {
    private AgentHistoryValue() {}

    static Map<String, Object> object(Object value) {
        Map<String, Object> out = objectOrNull(value);
        return out == null ? Collections.emptyMap() : out;
    }

    static Map<String, Object> objectOrNull(Object value) {
        if (!(value instanceof Map<?, ?>)) {
            return null;
        }
        Map<?, ?> map = (Map<?, ?>) value;
        Map<String, Object> out = new LinkedHashMap<>();
        for (Map.Entry<?, ?> entry : map.entrySet()) {
            out.put(String.valueOf(entry.getKey()), entry.getValue());
        }
        return Collections.unmodifiableMap(out);
    }

    static Map<String, Object> objectAt(Map<String, Object> map, String key) {
        return object(map.get(key));
    }

    static Map<String, Object> objectAtOrNull(Map<String, Object> map, String key) {
        return objectOrNull(map.get(key));
    }

    static List<Object> rawList(Object value) {
        if (!(value instanceof List<?>)) {
            return Collections.emptyList();
        }
        return Collections.unmodifiableList(new ArrayList<>((List<?>) value));
    }

    static List<String> stringList(Object value) {
        List<Object> raw = rawList(value);
        List<String> out = new ArrayList<>();
        for (Object item : raw) {
            String text = string(item);
            if (text != null) {
                out.add(text);
            }
        }
        return Collections.unmodifiableList(out);
    }

    static <T> List<T> objectList(Object value, Function<Map<String, Object>, T> mapper) {
        List<Object> raw = rawList(value);
        List<T> out = new ArrayList<>();
        for (Object item : raw) {
            Map<String, Object> object = objectOrNull(item);
            if (object != null) {
                out.add(mapper.apply(object));
            }
        }
        return Collections.unmodifiableList(out);
    }

    static String string(Object value) {
        return value == null ? null : String.valueOf(value);
    }

    static Boolean bool(Object value) {
        return value instanceof Boolean ? (Boolean) value : null;
    }

    static Integer integer(Object value) {
        if (value instanceof Number) {
            return Integer.valueOf(((Number) value).intValue());
        }
        return null;
    }

    static Long longValue(Object value) {
        if (value instanceof Number) {
            return Long.valueOf(((Number) value).longValue());
        }
        return null;
    }

    static Double doubleValue(Object value) {
        if (value instanceof Number) {
            return Double.valueOf(((Number) value).doubleValue());
        }
        return null;
    }

    static Map<String, Object> copyObject(Map<String, Object> input) {
        Map<String, Object> out = new LinkedHashMap<>();
        for (Map.Entry<String, Object> entry : input.entrySet()) {
            out.put(entry.getKey(), copy(entry.getValue()));
        }
        return Collections.unmodifiableMap(out);
    }

    static List<Object> copyList(List<?> input) {
        List<Object> out = new ArrayList<>();
        for (Object item : input) {
            out.add(copy(item));
        }
        return Collections.unmodifiableList(out);
    }

    static Object copy(Object value) {
        if (value instanceof Map<?, ?>) {
            Map<?, ?> map = (Map<?, ?>) value;
            Map<String, Object> out = new LinkedHashMap<>();
            for (Map.Entry<?, ?> entry : map.entrySet()) {
                out.put(String.valueOf(entry.getKey()), copy(entry.getValue()));
            }
            return Collections.unmodifiableMap(out);
        }
        if (value instanceof List<?>) {
            return copyList((List<?>) value);
        }
        return value;
    }

    static Map<String, Object> camelizeObject(Map<String, Object> input) {
        @SuppressWarnings("unchecked")
        Map<String, Object> out = (Map<String, Object>) camelize(input);
        return out;
    }

    static Object camelize(Object value) {
        if (value instanceof Map<?, ?>) {
            Map<?, ?> map = (Map<?, ?>) value;
            Map<String, Object> out = new LinkedHashMap<>();
            for (Map.Entry<?, ?> entry : map.entrySet()) {
                String key = snakeToCamel(String.valueOf(entry.getKey()));
                if ("databasePath".equals(key)
                        || "configPath".equals(key)
                        || "itemType".equals(key)
                        || "payloadType".equals(key)
                        || "recordType".equals(key)
                        || "semanticWeight".equals(key)
                        || "semanticFallbackCode".equals(key)
                        || "semanticFallback".equals(key)) {
                    continue;
                }
                out.put(key, camelize(entry.getValue()));
            }
            return Collections.unmodifiableMap(out);
        }
        if (value instanceof List<?>) {
            List<?> list = (List<?>) value;
            List<Object> out = new ArrayList<>();
            for (Object item : list) {
                out.add(camelize(item));
            }
            return Collections.unmodifiableList(out);
        }
        return value;
    }

    private static String snakeToCamel(String value) {
        StringBuilder out = new StringBuilder();
        boolean upper = false;
        for (int i = 0; i < value.length(); i++) {
            char c = value.charAt(i);
            if (c == '_') {
                upper = true;
            } else if (upper) {
                out.append(Character.toUpperCase(c));
                upper = false;
            } else {
                out.append(c);
            }
        }
        return out.toString();
    }
}
