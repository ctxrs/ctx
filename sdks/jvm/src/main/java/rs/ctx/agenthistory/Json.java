package rs.ctx.agenthistory;

import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

final class Json {
    private Json() {}

    static Map<String, Object> parseObject(String json) {
        Object value = new Parser(json).parse();
        if (!(value instanceof Map)) {
            throw new IllegalArgumentException("expected JSON object");
        }
        @SuppressWarnings("unchecked")
        Map<String, Object> object = (Map<String, Object>) value;
        return object;
    }

    static String stringify(Object value) {
        StringBuilder out = new StringBuilder();
        write(value, out);
        return out.toString();
    }

    private static void write(Object value, StringBuilder out) {
        if (value == null) { out.append("null"); return; }
        if (value instanceof String) { writeString((String) value, out); return; }
        if (value instanceof Number || value instanceof Boolean) { out.append(value); return; }
        if (value instanceof Map<?, ?>) {
            out.append('{'); boolean first = true;
            for (Map.Entry<?, ?> entry : ((Map<?, ?>) value).entrySet()) {
                if (!first) out.append(','); first = false;
                writeString(String.valueOf(entry.getKey()), out); out.append(':'); write(entry.getValue(), out);
            }
            out.append('}'); return;
        }
        if (value instanceof Iterable<?>) {
            out.append('['); boolean first = true;
            for (Object item : (Iterable<?>) value) { if (!first) out.append(','); first = false; write(item, out); }
            out.append(']'); return;
        }
        throw new IllegalArgumentException("unsupported JSON value: " + value.getClass().getName());
    }

    private static void writeString(String value, StringBuilder out) {
        out.append('"');
        for (int i = 0; i < value.length(); i++) {
            char ch = value.charAt(i);
            switch (ch) {
                case '"': out.append("\\\""); break;
                case '\\': out.append("\\\\"); break;
                case '\b': out.append("\\b"); break;
                case '\f': out.append("\\f"); break;
                case '\n': out.append("\\n"); break;
                case '\r': out.append("\\r"); break;
                case '\t': out.append("\\t"); break;
                default:
                    if (ch < 0x20) out.append(String.format("\\u%04x", Integer.valueOf(ch)));
                    else out.append(ch);
            }
        }
        out.append('"');
    }

    private static final class Parser {
        private final String input;
        private int index;

        Parser(String input) {
            this.input = input == null ? "" : input;
        }

        Object parse() {
            Object value = parseValue();
            skipWhitespace();
            if (index != input.length()) {
                throw error("trailing data");
            }
            return value;
        }

        private Object parseValue() {
            skipWhitespace();
            if (index >= input.length()) {
                throw error("unexpected end of input");
            }
            char ch = input.charAt(index);
            if (ch == '{') {
                return parseObjectValue();
            }
            if (ch == '[') {
                return parseArray();
            }
            if (ch == '"') {
                return parseString();
            }
            if (ch == 't') {
                expect("true");
                return Boolean.TRUE;
            }
            if (ch == 'f') {
                expect("false");
                return Boolean.FALSE;
            }
            if (ch == 'n') {
                expect("null");
                return null;
            }
            if (ch == '-' || Character.isDigit(ch)) {
                return parseNumber();
            }
            throw error("unexpected character");
        }

        private Map<String, Object> parseObjectValue() {
            expect('{');
            Map<String, Object> object = new LinkedHashMap<>();
            skipWhitespace();
            if (peek('}')) {
                index++;
                return object;
            }
            while (true) {
                skipWhitespace();
                String key = parseString();
                skipWhitespace();
                expect(':');
                Object value = parseValue();
                object.put(key, value);
                skipWhitespace();
                if (peek('}')) {
                    index++;
                    return object;
                }
                expect(',');
            }
        }

        private List<Object> parseArray() {
            expect('[');
            List<Object> array = new ArrayList<>();
            skipWhitespace();
            if (peek(']')) {
                index++;
                return array;
            }
            while (true) {
                array.add(parseValue());
                skipWhitespace();
                if (peek(']')) {
                    index++;
                    return array;
                }
                expect(',');
            }
        }

        private String parseString() {
            expect('"');
            StringBuilder out = new StringBuilder();
            while (index < input.length()) {
                char ch = input.charAt(index++);
                if (ch == '"') {
                    return out.toString();
                }
                if (ch == '\\') {
                    if (index >= input.length()) {
                        throw error("unterminated escape");
                    }
                    char escaped = input.charAt(index++);
                    switch (escaped) {
                        case '"':
                        case '\\':
                        case '/':
                            out.append(escaped);
                            break;
                        case 'b':
                            out.append('\b');
                            break;
                        case 'f':
                            out.append('\f');
                            break;
                        case 'n':
                            out.append('\n');
                            break;
                        case 'r':
                            out.append('\r');
                            break;
                        case 't':
                            out.append('\t');
                            break;
                        case 'u':
                            out.append(parseUnicode());
                            break;
                        default:
                            throw error("invalid escape");
                    }
                } else {
                    out.append(ch);
                }
            }
            throw error("unterminated string");
        }

        private char parseUnicode() {
            if (index + 4 > input.length()) {
                throw error("invalid unicode escape");
            }
            String hex = input.substring(index, index + 4);
            index += 4;
            try {
                return (char) Integer.parseInt(hex, 16);
            } catch (NumberFormatException cause) {
                throw error("invalid unicode escape");
            }
        }

        private Number parseNumber() {
            int start = index;
            if (peek('-')) {
                index++;
            }
            readDigits();
            boolean decimal = false;
            if (peek('.')) {
                decimal = true;
                index++;
                readDigits();
            }
            if (peek('e') || peek('E')) {
                decimal = true;
                index++;
                if (peek('+') || peek('-')) {
                    index++;
                }
                readDigits();
            }
            String number = input.substring(start, index);
            try {
                return decimal ? Double.valueOf(number) : Long.valueOf(number);
            } catch (NumberFormatException cause) {
                throw error("invalid number");
            }
        }

        private void readDigits() {
            int start = index;
            while (index < input.length() && Character.isDigit(input.charAt(index))) {
                index++;
            }
            if (start == index) {
                throw error("expected digit");
            }
        }

        private void expect(String text) {
            if (!input.startsWith(text, index)) {
                throw error("expected " + text);
            }
            index += text.length();
        }

        private void expect(char ch) {
            if (!peek(ch)) {
                throw error("expected " + ch);
            }
            index++;
        }

        private boolean peek(char ch) {
            return index < input.length() && input.charAt(index) == ch;
        }

        private void skipWhitespace() {
            while (index < input.length()) {
                char ch = input.charAt(index);
                if (ch == ' ' || ch == '\n' || ch == '\r' || ch == '\t') {
                    index++;
                } else {
                    return;
                }
            }
        }

        private IllegalArgumentException error(String message) {
            return new IllegalArgumentException(message + " at byte " + index);
        }
    }
}
