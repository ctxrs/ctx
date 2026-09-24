pub fn shebang_language(source: &str) -> Option<&'static str> {
    let mut words = source
        .lines()
        .next()?
        .strip_prefix("#!")?
        .split_whitespace();
    let interpreter = words.next()?.rsplit('/').next()?;
    let interpreter = if interpreter == "env" {
        let mut command = words.next()?;
        if matches!(command, "-S" | "--") {
            command = words.next()?;
        }
        if command.starts_with('-') || command.contains('=') {
            return None;
        }
        command.rsplit('/').next()?
    } else {
        interpreter
    };
    Some(match interpreter {
        "bash" | "sh" | "dash" => "bash",
        "ruby" => "ruby",
        "php" => "php",
        "lua" | "luajit" => "lua",
        "luau" => "luau",
        "pwsh" | "powershell" => "powershell",
        "elixir" => "elixir",
        "node" | "nodejs" => "javascript",
        "julia" => "julia",
        "python" | "python2" | "python3" => "python",
        _ => return None,
    })
}
