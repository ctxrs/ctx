use std::{
    fs,
    io::{self, Read},
    path::Path,
};

use anyhow::{anyhow, Context, Result};
use ctx_protocol::{SearchClause, SearchQueryV1, SearchQueryVersion, SEARCH_MAX_QUERY_JSON_BYTES};
use serde_json::Value;

use crate::SearchArgs;

pub(crate) fn search_query_from_args(args: &SearchArgs) -> Result<Option<SearchQueryV1>> {
    build_search_query(SearchQueryInput {
        positional: args.query.as_deref(),
        terms: &args.term,
        phrases: &args.phrase,
        literals: &args.literal,
        semantic: args.semantic.as_deref(),
        must: &args.must,
        exclude: &args.exclude,
        query_file: args.query_file.as_deref(),
        query_json: args.query_json.as_deref(),
        has_file_filter: args.file.is_some(),
    })
}

pub(crate) fn parse_search_query_value(value: &Value) -> Result<SearchQueryV1> {
    let serialized = serde_json::to_vec(value).context("failed to encode search query")?;
    parse_search_query_json(&serialized)
}

pub(crate) fn parse_search_query_json(bytes: &[u8]) -> Result<SearchQueryV1> {
    if bytes.len() > SEARCH_MAX_QUERY_JSON_BYTES {
        return Err(anyhow!(
            "search query JSON is {} bytes; maximum is {}",
            bytes.len(),
            SEARCH_MAX_QUERY_JSON_BYTES
        ));
    }
    serde_json::from_slice::<SearchQueryV1>(bytes)
        .context("invalid ctx-search-v1 query JSON")?
        .canonicalized()
        .map_err(Into::into)
}

fn read_search_query_file(path: &Path) -> Result<Vec<u8>> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to inspect search query file {}", path.display()))?;
    if !metadata.is_file() {
        return Err(anyhow!(
            "search query file {} is not a regular file",
            path.display()
        ));
    }
    let file_len = metadata.len();
    if file_len > SEARCH_MAX_QUERY_JSON_BYTES as u64 {
        return Err(anyhow!(
            "search query file is {file_len} bytes; maximum is {SEARCH_MAX_QUERY_JSON_BYTES}"
        ));
    }

    let file = open_search_query_file(path)
        .with_context(|| format!("failed to read search query file {}", path.display()))?;
    if !file
        .metadata()
        .with_context(|| format!("failed to inspect search query file {}", path.display()))?
        .is_file()
    {
        return Err(anyhow!(
            "search query file {} is not a regular file",
            path.display()
        ));
    }

    read_search_query_bytes(file, file_len)
        .with_context(|| format!("failed to read search query file {}", path.display()))
}

fn read_search_query_bytes(reader: impl Read, initial_len: u64) -> Result<Vec<u8>> {
    const PROBE_LEN: usize = SEARCH_MAX_QUERY_JSON_BYTES + 1;
    let initial_capacity = usize::try_from(initial_len)
        .unwrap_or(SEARCH_MAX_QUERY_JSON_BYTES)
        .min(SEARCH_MAX_QUERY_JSON_BYTES);
    let mut bytes = Vec::with_capacity(initial_capacity);
    reader
        .take(PROBE_LEN as u64)
        .read_to_end(&mut bytes)
        .context("failed to read search query bytes")?;
    if bytes.len() > SEARCH_MAX_QUERY_JSON_BYTES {
        return Err(anyhow!(
            "search query file exceeds maximum of {SEARCH_MAX_QUERY_JSON_BYTES} bytes"
        ));
    }
    Ok(bytes)
}

fn open_search_query_file(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.custom_flags(libc::O_NONBLOCK);
    }
    options.open(path)
}

struct SearchQueryInput<'a> {
    positional: Option<&'a str>,
    terms: &'a [String],
    phrases: &'a [String],
    literals: &'a [String],
    semantic: Option<&'a str>,
    must: &'a [String],
    exclude: &'a [String],
    query_file: Option<&'a Path>,
    query_json: Option<&'a str>,
    has_file_filter: bool,
}

fn build_search_query(input: SearchQueryInput<'_>) -> Result<Option<SearchQueryV1>> {
    let has_construction_flags = input.positional.is_some()
        || !input.terms.is_empty()
        || !input.phrases.is_empty()
        || !input.literals.is_empty()
        || input.semantic.is_some()
        || !input.must.is_empty()
        || !input.exclude.is_empty();
    if input.query_file.is_some() && (input.query_json.is_some() || has_construction_flags) {
        return Err(anyhow!(
            "--query-file cannot be combined with a positional query or query construction flags"
        ));
    }
    if input.query_json.is_some() && has_construction_flags {
        return Err(anyhow!(
            "structured query JSON cannot be combined with a positional query or query construction flags"
        ));
    }
    if let Some(path) = input.query_file {
        let bytes = read_search_query_file(path)?;
        return parse_search_query_json(&bytes)
            .with_context(|| format!("invalid search query file {}", path.display()))
            .map(Some);
    }
    if let Some(json) = input.query_json {
        return parse_search_query_json(json.as_bytes()).map(Some);
    }
    if !has_construction_flags {
        return if input.has_file_filter {
            Ok(None)
        } else {
            Err(anyhow!(
                "search needs a query, --term, --phrase, --literal, --semantic, --must, --query-file, or --file"
            ))
        };
    }

    let mut any = Vec::new();
    if let Some(positional) = input.positional {
        any.push(SearchClause::all(positional));
    }
    any.extend(input.terms.iter().cloned().map(SearchClause::all));
    any.extend(input.phrases.iter().cloned().map(SearchClause::phrase));
    any.extend(input.literals.iter().cloned().map(SearchClause::literal));
    if let Some(semantic) = input.semantic {
        any.push(SearchClause::semantic(semantic));
    }
    SearchQueryV1 {
        version: SearchQueryVersion::V1,
        any,
        must: input.must.iter().cloned().map(SearchClause::all).collect(),
        must_not: input
            .exclude
            .iter()
            .cloned()
            .map(SearchClause::all)
            .collect(),
    }
    .canonicalized()
    .map(Some)
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use std::{
        fs::OpenOptions,
        io::{Seek, Write},
    };

    use super::*;

    fn input<'a>() -> SearchQueryInput<'a> {
        SearchQueryInput {
            positional: None,
            terms: &[],
            phrases: &[],
            literals: &[],
            semantic: None,
            must: &[],
            exclude: &[],
            query_file: None,
            query_json: None,
            has_file_filter: false,
        }
    }

    #[test]
    fn constructs_cli_clauses_with_explicit_placements() {
        let terms = vec!["disk io pressure".to_owned(), "storage latency".to_owned()];
        let phrases = vec!["small writes".to_owned()];
        let literals = vec!["logs_2.db".to_owned()];
        let must = vec!["codex worker".to_owned()];
        let exclude = vec!["postgres vacuum".to_owned()];
        let query = build_search_query(SearchQueryInput {
            terms: &terms,
            phrases: &phrases,
            literals: &literals,
            semantic: Some("machine became sluggish"),
            must: &must,
            exclude: &exclude,
            ..input()
        })
        .unwrap()
        .unwrap();
        assert_eq!(
            query.any,
            vec![
                SearchClause::all("disk io pressure"),
                SearchClause::all("storage latency"),
                SearchClause::phrase("small writes"),
                SearchClause::literal("logs_2.db"),
                SearchClause::semantic("machine became sluggish"),
            ]
        );
        assert_eq!(query.must, vec![SearchClause::all("codex worker")]);
        assert_eq!(query.must_not, vec![SearchClause::all("postgres vacuum")]);
    }

    #[test]
    fn positional_is_one_all_words_clause() {
        let query = build_search_query(SearchQueryInput {
            positional: Some("disk io pressure"),
            ..input()
        })
        .unwrap();
        assert_eq!(
            query.unwrap().any,
            vec![SearchClause::all("disk io pressure")]
        );
    }

    #[test]
    fn rejects_negative_only_input() {
        let exclude = vec!["noise".to_owned()];
        assert!(build_search_query(SearchQueryInput {
            exclude: &exclude,
            ..input()
        })
        .is_err());
    }

    #[test]
    fn structured_json_uses_the_same_parser_and_validation() {
        let query = parse_search_query_json(
            br#"{"version":"ctx-search-v1","any":[{"semantic":" disk pressure "}]}"#,
        )
        .unwrap();
        assert_eq!(query.any, vec![SearchClause::semantic("disk pressure")]);
        assert!(parse_search_query_json(
            br#"{"version":"ctx-search-v1","must":[{"semantic":"noise"}]}"#
        )
        .is_err());
    }

    #[test]
    fn permits_file_only_but_not_empty_search() {
        assert_eq!(
            build_search_query(SearchQueryInput {
                has_file_filter: true,
                ..input()
            })
            .unwrap(),
            None
        );
        assert!(build_search_query(input()).is_err());
    }

    #[test]
    fn query_reader_stops_after_one_byte_beyond_the_limit() {
        let error = read_search_query_bytes(io::repeat(b'x'), 0).unwrap_err();
        assert!(error.to_string().contains("exceeds maximum"));
    }

    #[test]
    fn query_file_path_enforces_the_exact_byte_boundary() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("query.json");
        let mut bytes = br#"{"version":"ctx-search-v1","any":[{"all":"disk io"}]}"#.to_vec();
        bytes.resize(SEARCH_MAX_QUERY_JSON_BYTES, b' ');
        fs::write(&path, &bytes).unwrap();

        let loaded = read_search_query_file(&path).unwrap();
        assert_eq!(loaded.len(), SEARCH_MAX_QUERY_JSON_BYTES);
        parse_search_query_json(&loaded).unwrap();

        bytes.push(b' ');
        fs::write(&path, &bytes).unwrap();
        assert!(read_search_query_file(&path).is_err());
    }

    #[test]
    fn query_file_is_bounded_when_it_grows_after_inspection() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("query.json");
        fs::write(
            &path,
            br#"{"version":"ctx-search-v1","any":[{"all":"disk io"}]}"#,
        )
        .unwrap();
        let mut reader = fs::File::open(&path).unwrap();
        let initial_len = reader.metadata().unwrap().len();
        let mut writer = OpenOptions::new().append(true).open(&path).unwrap();
        writer
            .write_all(&vec![b' '; SEARCH_MAX_QUERY_JSON_BYTES])
            .unwrap();
        writer.flush().unwrap();
        reader.rewind().unwrap();
        assert!(read_search_query_bytes(reader, initial_len).is_err());
    }
}
