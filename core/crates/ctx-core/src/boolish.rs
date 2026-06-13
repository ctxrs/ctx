pub fn parse_boolish(raw: &str) -> Option<bool> {
    let normalized = raw.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::parse_boolish;

    #[test]
    fn accepts_true_values() {
        for raw in ["1", "true", "TRUE", " yes ", "On"] {
            assert_eq!(parse_boolish(raw), Some(true), "raw={raw}");
        }
    }

    #[test]
    fn accepts_false_values() {
        for raw in ["0", "false", "FALSE", " no ", "Off"] {
            assert_eq!(parse_boolish(raw), Some(false), "raw={raw}");
        }
    }

    #[test]
    fn rejects_invalid_values() {
        for raw in ["", " ", "maybe", "enable", "disabled"] {
            assert_eq!(parse_boolish(raw), None, "raw={raw}");
        }
    }
}
