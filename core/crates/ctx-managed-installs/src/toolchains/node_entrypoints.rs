use std::io::BufRead;
use std::path::Path;

pub fn archive_bin_requires_node_runtime(bin_path: &str, installed_bin_path: &Path) -> bool {
    let ext = Path::new(bin_path)
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase());
    if matches!(ext.as_deref(), Some("js") | Some("mjs") | Some("cjs")) {
        return true;
    }
    archive_bin_has_node_shebang(installed_bin_path)
}

fn archive_bin_has_node_shebang(path: &Path) -> bool {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return false,
    };
    let mut reader = std::io::BufReader::new(file);
    let mut first_line = String::new();
    let bytes = match reader.read_line(&mut first_line) {
        Ok(bytes) => bytes,
        Err(_) => return false,
    };
    if bytes == 0 {
        return false;
    }
    shebang_invokes_node(first_line.trim())
}

fn shebang_invokes_node(line: &str) -> bool {
    let Some(shebang) = line.strip_prefix("#!") else {
        return false;
    };
    let mut tokens = shebang.split_whitespace();
    let Some(program) = tokens.next() else {
        return false;
    };
    if shebang_token_is_node(program) {
        return true;
    }
    if !shebang_token_is_env(program) {
        return false;
    }
    for token in tokens {
        if token.starts_with('-') || token.contains('=') {
            continue;
        }
        return shebang_token_is_node(token);
    }
    false
}

fn shebang_token_is_env(token: &str) -> bool {
    let base = shebang_token_basename(token);
    base.eq_ignore_ascii_case("env")
}

fn shebang_token_is_node(token: &str) -> bool {
    let base = shebang_token_basename(token);
    base.eq_ignore_ascii_case("node") || base.eq_ignore_ascii_case("node.exe")
}

fn shebang_token_basename(token: &str) -> &str {
    token
        .trim_matches('"')
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(token)
}
