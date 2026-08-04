package rs.ctx.agenthistory;

import java.math.BigDecimal;
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
            if (ch == '-' || isDigit(ch)) {
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
                if (object.containsKey(key)) {
                    throw error("duplicate JSON object member " + key);
                }
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
                } else if (ch <= 0x1f) {
                    throw error("unescaped control character in string");
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
            if (peek('0')) {
                index++;
                if (index < input.length() && isDigit(input.charAt(index))) {
                    throw error("leading zero in number");
                }
            } else {
                readDigits();
            }
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
                return decimal ? new BigDecimal(number) : Long.valueOf(number);
            } catch (NumberFormatException cause) {
                throw error("invalid number");
            }
        }

        private void readDigits() {
            int start = index;
            while (index < input.length() && isDigit(input.charAt(index))) {
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

        private static boolean isDigit(char ch) {
            return ch >= '0' && ch <= '9';
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
