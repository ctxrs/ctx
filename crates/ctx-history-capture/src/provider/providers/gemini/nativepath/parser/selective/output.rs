use super::*;

impl GeminiRawJson<'_> {
    fn output_value(&mut self, depth: usize) -> std::result::Result<GeminiRawOutput, String> {
        if depth > MAX_GEMINI_STRUCTURAL_DEPTH {
            return Err(format!(
                "Gemini result JSON exceeds structural depth {MAX_GEMINI_STRUCTURAL_DEPTH}"
            ));
        }
        self.whitespace();
        match self.peek() {
            Some(b'"') => {
                let value = self.string(MAX_PROVIDER_JSONL_LINE_BYTES)?;
                Ok(GeminiRawOutput {
                    content: GeminiSelectedContent::String {
                        value: value.retained,
                        sha256: value.sha256,
                    },
                    ..GeminiRawOutput::default()
                })
            }
            Some(b'{') => {
                let start = self.offset;
                let mut output = self.output_object(depth.saturating_add(1))?;
                if matches!(output.content, GeminiSelectedContent::Absent) {
                    output.content = structured_content(&self.bytes[start..self.offset])?;
                }
                Ok(output)
            }
            Some(b'n') => {
                self.consume_literal(b"null")?;
                Ok(GeminiRawOutput {
                    content: GeminiSelectedContent::Null,
                    ..GeminiRawOutput::default()
                })
            }
            Some(_) => {
                let start = self.offset;
                self.skip_value(depth)?;
                let encoded = &self.bytes[start..self.offset];
                let mut hasher = Sha256::new();
                hasher.update(RESULT_STRUCTURED_HASH_DOMAIN);
                hasher.update(encoded);
                Ok(GeminiRawOutput {
                    content: GeminiSelectedContent::Structured {
                        value: serde_json::from_slice(encoded)
                            .map_err(|error| format!("invalid Gemini result value: {error}"))?,
                        sha256: hasher.finalize().into(),
                    },
                    ..GeminiRawOutput::default()
                })
            }
            None => Err("missing Gemini output value".to_owned()),
        }
    }

    fn output_object(&mut self, depth: usize) -> std::result::Result<GeminiRawOutput, String> {
        self.take(b'{')?;
        self.whitespace();
        let mut output = GeminiRawOutput::default();
        let mut content = None;
        let mut output_alias = None;
        let mut text = None;
        let mut unknown_member = false;
        if self.peek() == Some(b'}') {
            self.take(b'}')?;
            return Ok(output);
        }
        loop {
            let key = self.key()?;
            self.whitespace();
            self.take(b':')?;
            self.whitespace();
            match key.as_deref() {
                Some("content") => {
                    content = Some(self.content_candidate(depth)?);
                }
                Some("output") => {
                    output_alias = Some(self.content_candidate(depth)?);
                }
                Some("text") => {
                    text = Some(self.content_candidate(depth)?);
                }
                Some(key) => {
                    if !self.outcome_field(key, &mut output.outcome, depth)? {
                        unknown_member = true;
                        let nested = self.nested_outcome_value(depth.saturating_add(1))?;
                        output.outcome.merge_nested(nested.outcome);
                    }
                }
                None => {
                    unknown_member = true;
                    let nested = self.nested_outcome_value(depth.saturating_add(1))?;
                    output.outcome.merge_nested(nested.outcome);
                }
            }
            self.whitespace();
            match self.peek() {
                Some(b',') => {
                    self.take(b',')?;
                    self.whitespace();
                }
                Some(b'}') => {
                    self.take(b'}')?;
                    break;
                }
                _ => {
                    return Err(format!(
                        "invalid Gemini result object near byte {}",
                        self.offset
                    ));
                }
            }
        }
        let payload_member_count = usize::from(content.is_some())
            + usize::from(output_alias.is_some())
            + usize::from(text.is_some());
        output.known_envelope = payload_member_count > 0;
        output.unknown_envelope_member =
            output.known_envelope && (payload_member_count != 1 || unknown_member);
        output.content = content.or(output_alias).or(text).unwrap_or_default();
        Ok(output)
    }

    fn nested_outcome_value(
        &mut self,
        depth: usize,
    ) -> std::result::Result<GeminiRawOutput, String> {
        if depth > MAX_GEMINI_STRUCTURAL_DEPTH {
            return Err(format!(
                "Gemini result JSON exceeds structural depth {MAX_GEMINI_STRUCTURAL_DEPTH}"
            ));
        }
        self.whitespace();
        let mut output = GeminiRawOutput::default();
        match self.peek() {
            Some(b'{') => {
                self.take(b'{')?;
                self.whitespace();
                if self.peek() == Some(b'}') {
                    self.take(b'}')?;
                    return Ok(output);
                }
                loop {
                    let key = self.key()?;
                    self.whitespace();
                    self.take(b':')?;
                    self.whitespace();
                    match key.as_deref() {
                        Some(key) if self.outcome_field(key, &mut output.outcome, depth)? => {}
                        _ => {
                            let nested = self.nested_outcome_value(depth.saturating_add(1))?;
                            output.outcome.merge_nested(nested.outcome);
                        }
                    }
                    self.whitespace();
                    match self.peek() {
                        Some(b',') => {
                            self.take(b',')?;
                            self.whitespace();
                        }
                        Some(b'}') => {
                            self.take(b'}')?;
                            break;
                        }
                        _ => {
                            return Err(format!(
                                "invalid Gemini result object near byte {}",
                                self.offset
                            ));
                        }
                    }
                }
            }
            Some(b'[') => {
                self.take(b'[')?;
                self.whitespace();
                if self.peek() == Some(b']') {
                    self.take(b']')?;
                    return Ok(output);
                }
                loop {
                    let nested = self.nested_outcome_value(depth.saturating_add(1))?;
                    output.outcome.merge_nested(nested.outcome);
                    self.whitespace();
                    match self.peek() {
                        Some(b',') => {
                            self.take(b',')?;
                            self.whitespace();
                        }
                        Some(b']') => {
                            self.take(b']')?;
                            break;
                        }
                        _ => {
                            return Err(format!(
                                "invalid Gemini result array near byte {}",
                                self.offset
                            ));
                        }
                    }
                }
            }
            Some(_) => self.skip_value(depth)?,
            None => return Err("missing Gemini nested outcome value".to_owned()),
        }
        Ok(output)
    }

    fn content_candidate(
        &mut self,
        depth: usize,
    ) -> std::result::Result<GeminiSelectedContent, String> {
        self.whitespace();
        match self.peek() {
            Some(b'"') => self.string(MAX_PROVIDER_JSONL_LINE_BYTES).map(|content| {
                GeminiSelectedContent::String {
                    value: content.retained,
                    sha256: content.sha256,
                }
            }),
            Some(b'n') => {
                self.consume_literal(b"null")?;
                Ok(GeminiSelectedContent::Null)
            }
            _ => {
                let start = self.offset;
                self.skip_value(depth)?;
                let encoded = &self.bytes[start..self.offset];
                let mut hasher = Sha256::new();
                hasher.update(RESULT_STRUCTURED_HASH_DOMAIN);
                hasher.update(encoded);
                Ok(GeminiSelectedContent::Structured {
                    value: serde_json::from_slice(encoded)
                        .map_err(|error| format!("invalid Gemini result value: {error}"))?,
                    sha256: hasher.finalize().into(),
                })
            }
        }
    }

    fn outcome_field(
        &mut self,
        key: &str,
        outcome: &mut GeminiOutputOutcomeDto,
        depth: usize,
    ) -> std::result::Result<bool, String> {
        match key {
            "error" => {
                outcome.diagnostic_member = true;
                outcome.error = FailureMarker(self.failure_marker(depth)?);
            }
            "warning" | "warnings" | "stderr" | "diagnostic" | "diagnostics" => {
                outcome.diagnostic_member = true;
                self.skip_value(depth)?;
            }
            "success" => {
                outcome.success = BoolMarker(self.bool_marker(depth)?);
                outcome.framing_unknown |= outcome.success.0.is_none();
            }
            "ok" => {
                outcome.ok = BoolMarker(self.bool_marker(depth)?);
                outcome.framing_unknown |= outcome.ok.0.is_none();
            }
            "status" => outcome.status = self.status_marker(depth)?,
            "state" => outcome.state = self.status_marker(depth)?,
            "outcome" => outcome.outcome = self.status_marker(depth)?,
            "isError" | "is_error" => {
                outcome.is_error = BoolMarker(self.bool_marker(depth)?);
                outcome.framing_unknown |= outcome.is_error.0.is_none();
            }
            "timedOut" | "timed_out" => {
                outcome.timed_out = BoolMarker(self.bool_marker(depth)?);
                outcome.framing_unknown |= outcome.timed_out.0.is_none();
            }
            "timeout" => {
                outcome.timeout = BoolMarker(self.bool_marker(depth)?);
                outcome.framing_unknown |= outcome.timeout.0.is_none();
            }
            "exitCode" | "exit_code" => {
                outcome.exit_code = I64Marker(self.i64_marker(depth)?);
                outcome.framing_unknown |= outcome.exit_code.0.is_none();
            }
            "statusCode" | "status_code" => {
                outcome.status_code = I64Marker(self.i64_marker(depth)?);
                outcome.framing_unknown |= outcome.status_code.0.is_none();
            }
            "durationMs" | "duration_ms" | "duration" => {
                outcome.duration_ms = U64Marker(self.u64_marker(depth)?);
            }
            "redacted" => {
                outcome.redacted = RedactionMarker(self.redaction_marker(depth)?);
            }
            "isRedacted" | "is_redacted" => {
                outcome.is_redacted = RedactionMarker(self.redaction_marker(depth)?);
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn bool_marker(&mut self, depth: usize) -> std::result::Result<Option<bool>, String> {
        self.whitespace();
        match self.peek() {
            Some(b't') => {
                self.consume_literal(b"true")?;
                Ok(Some(true))
            }
            Some(b'f') => {
                self.consume_literal(b"false")?;
                Ok(Some(false))
            }
            _ => {
                self.skip_value(depth)?;
                Ok(None)
            }
        }
    }

    fn redaction_marker(&mut self, depth: usize) -> std::result::Result<bool, String> {
        self.whitespace();
        if self.peek() == Some(b'f') {
            self.consume_literal(b"false")?;
            Ok(false)
        } else {
            self.skip_value(depth)?;
            Ok(true)
        }
    }

    fn failure_marker(&mut self, depth: usize) -> std::result::Result<bool, String> {
        self.whitespace();
        match self.peek() {
            Some(b'n') => {
                self.consume_literal(b"null")?;
                Ok(false)
            }
            Some(b't') => {
                self.consume_literal(b"true")?;
                Ok(true)
            }
            Some(b'f') => {
                self.consume_literal(b"false")?;
                Ok(false)
            }
            Some(b'"') => {
                let value = self.string(64)?;
                Ok(value.non_whitespace)
            }
            Some(b'{') | Some(b'[') => {
                let start = self.offset;
                self.skip_value(depth)?;
                let end = self.offset;
                Ok(self.bytes[start.saturating_add(1)..end.saturating_sub(1)]
                    .iter()
                    .any(|byte| !byte.is_ascii_whitespace()))
            }
            Some(_) => {
                let number = self.number()?;
                Ok(number.parse::<i64>().is_ok_and(|value| value != 0))
            }
            None => Err("missing Gemini failure marker".to_owned()),
        }
    }

    fn status_marker(&mut self, depth: usize) -> std::result::Result<StatusMarker, String> {
        self.whitespace();
        if self.peek() != Some(b'"') {
            self.skip_value(depth)?;
            return Ok(StatusMarker {
                present: true,
                ..StatusMarker::default()
            });
        }
        let value = self.string(64)?;
        if value.truncated {
            return Ok(StatusMarker::default());
        }
        let redacted = matches!(value.retained.as_str(), "redacted" | "output-redacted");
        let status = value.retained.trim().to_ascii_lowercase();
        Ok(StatusMarker {
            success: matches!(
                status.as_str(),
                "success" | "succeeded" | "complete" | "completed" | "ok" | "passed"
            ),
            failure: matches!(
                status.as_str(),
                "failed"
                    | "failure"
                    | "error"
                    | "errored"
                    | "timeout"
                    | "timed_out"
                    | "timedout"
                    | "cancelled"
                    | "canceled"
            ),
            redacted,
            present: true,
        })
    }

    fn i64_marker(&mut self, depth: usize) -> std::result::Result<Option<i64>, String> {
        self.whitespace();
        if self
            .peek()
            .is_none_or(|byte| !(byte.is_ascii_digit() || byte == b'-'))
        {
            self.skip_value(depth)?;
            return Ok(None);
        }
        let number = self.number()?;
        Ok(number.parse::<i64>().ok().or_else(|| {
            number
                .parse::<u64>()
                .ok()
                .and_then(|value| value.try_into().ok())
        }))
    }

    fn u64_marker(&mut self, depth: usize) -> std::result::Result<Option<u64>, String> {
        self.whitespace();
        if self
            .peek()
            .is_none_or(|byte| !(byte.is_ascii_digit() || byte == b'-'))
        {
            self.skip_value(depth)?;
            return Ok(None);
        }
        let number = self.number()?;
        Ok(number.parse::<u64>().ok().or_else(|| {
            number
                .parse::<i64>()
                .ok()
                .and_then(|value| value.try_into().ok())
        }))
    }
}

struct GeminiRawResultCall {
    id: Option<String>,
    name: Option<String>,
    args: GeminiRepositoryArgs,
    result: Option<GeminiRawOutput>,
    outcome: GeminiOutputOutcomeDto,
}

pub(super) fn parse_result_record_selectively(
    payload: &[u8],
) -> std::result::Result<ProbedGeminiResult, String> {
    let mut parser = GeminiRawJson::new(payload);
    parser.whitespace();
    parser.take(b'{')?;
    parser.whitespace();
    let mut id = None;
    let mut timestamp = None;
    let mut top_result = None;
    let mut calls = Vec::new();
    let mut outcome = GeminiOutputOutcomeDto::default();
    let mut saw_id = false;
    let mut saw_timestamp = false;
    let mut saw_result = false;
    let mut saw_tool_calls = false;
    let mut aggregate_unknown = false;

    if parser.peek() != Some(b'}') {
        loop {
            let key = parser.key()?;
            parser.whitespace();
            parser.take(b':')?;
            parser.whitespace();
            match key.as_deref() {
                Some("id") => {
                    if saw_id {
                        return Err("duplicate id field in Gemini result record".to_owned());
                    }
                    saw_id = true;
                    id = parser.strict_optional_string()?;
                }
                Some("timestamp") => {
                    if saw_timestamp {
                        return Err("duplicate timestamp field in Gemini result record".to_owned());
                    }
                    saw_timestamp = true;
                    timestamp = parser.strict_optional_string()?;
                }
                Some("result") => {
                    if saw_result {
                        return Err("duplicate result field in Gemini result record".to_owned());
                    }
                    saw_result = true;
                    top_result = Some(parser.output_value(1)?);
                }
                Some("toolCalls") => {
                    if saw_tool_calls {
                        return Err("duplicate toolCalls field in Gemini result record".to_owned());
                    }
                    saw_tool_calls = true;
                    let parsed = parser.result_calls(1)?;
                    calls = parsed.0;
                    aggregate_unknown |= parsed.1;
                }
                Some("type") => parser.skip_value(1)?,
                Some(key) => {
                    if !parser.outcome_field(key, &mut outcome, 1)? {
                        aggregate_unknown = true;
                        parser.skip_value(1)?;
                    }
                }
                None => {
                    aggregate_unknown = true;
                    parser.skip_value(1)?;
                }
            }
            parser.whitespace();
            match parser.peek() {
                Some(b',') => {
                    parser.take(b',')?;
                    parser.whitespace();
                }
                Some(b'}') => break,
                _ => {
                    return Err(format!(
                        "invalid Gemini result object near byte {}",
                        parser.offset
                    ));
                }
            }
        }
    }
    parser.take(b'}')?;
    parser.whitespace();
    parser.finish()?;

    let mut outputs = Vec::new();
    let mut output_count = 0_usize;
    let record_redacted = outcome.is_redacted();
    if let Some(result) = top_result {
        output_count = output_count.saturating_add(1);
        outputs.push(finish_probed_output(
            None,
            None,
            GeminiRepositoryArgs::default(),
            false,
            &outcome,
            result,
        ));
    }
    for call in calls {
        let Some(result) = call.result else {
            continue;
        };
        if output_count >= MAX_GEMINI_NATIVE_PAGE_RECORDS {
            return Err(format!(
                "Gemini result record exceeds the {MAX_GEMINI_NATIVE_PAGE_RECORDS} output limit"
            ));
        }
        output_count = output_count.saturating_add(1);
        outputs.push(finish_probed_output(
            nonempty(call.id),
            nonempty(call.name),
            call.args,
            record_redacted,
            &call.outcome,
            result,
        ));
    }

    Ok(ProbedGeminiResult {
        native_record_id: nonempty(id),
        occurred_at_unix_ms: timestamp
            .as_deref()
            .and_then(parse_timestamp)
            .map(|timestamp| timestamp.timestamp_millis()),
        outputs,
        aggregate_unknown,
    })
}

pub(super) fn finish_probed_output(
    call_id: Option<String>,
    tool_name: Option<String>,
    repository_args: GeminiRepositoryArgs,
    record_redacted: bool,
    outer_outcome: &GeminiOutputOutcomeDto,
    result: GeminiRawOutput,
) -> ProbedGeminiOutput {
    let (content_kind, content_sha256) = match &result.content {
        GeminiSelectedContent::String { sha256, .. } => (b"string".as_slice(), Some(*sha256)),
        GeminiSelectedContent::Absent => (b"absent".as_slice(), None),
        GeminiSelectedContent::Null => (b"null".as_slice(), None),
        GeminiSelectedContent::Structured { sha256, .. } => {
            (b"structured".as_slice(), Some(*sha256))
        }
    };
    let outcome = outer_outcome.combined_metadata(&result.outcome);
    let terminal_status = outer_outcome.terminal_status_with(&result.outcome);
    let fallback_identity_sha256 = result_fallback_identity_sha256(
        call_id.as_deref(),
        tool_name.as_deref(),
        &outcome,
        content_kind,
        content_sha256.as_ref(),
    );
    let result_value = match result.content {
        GeminiSelectedContent::Absent => None,
        GeminiSelectedContent::String { value, .. } => Some(Value::String(value)),
        GeminiSelectedContent::Null => Some(Value::Null),
        GeminiSelectedContent::Structured { value, .. } => Some(value),
    };
    let mut atoms = Vec::new();
    if result.known_envelope {
        atoms.push(ResultAtom::KnownProviderEnvelope);
    }
    if result_value.is_some() {
        atoms.push(ResultAtom::Payload);
    }
    if outer_outcome.diagnostic_member
        || result.outcome.diagnostic_member
        || result_value.as_ref().is_some_and(has_structural_diagnostic)
    {
        atoms.push(ResultAtom::Diagnostic);
    }
    if result.unknown_envelope_member {
        atoms.push(ResultAtom::Unknown);
    }
    ProbedGeminiOutput {
        result: result_value,
        call_id,
        tool_name,
        command: repository_args.command,
        command_too_large: repository_args.command_too_large,
        declared_workdir: repository_args.declared_workdir,
        file_paths: repository_args.file_paths,
        ambiguous_native_fields: repository_args.ambiguous_native_fields,
        outcome,
        terminal_status,
        atoms,
        redacted: record_redacted || outer_outcome.redacted_with(&result.outcome),
        fallback_identity_sha256,
    }
}

fn structured_content(encoded: &[u8]) -> std::result::Result<GeminiSelectedContent, String> {
    let mut hasher = Sha256::new();
    hasher.update(RESULT_STRUCTURED_HASH_DOMAIN);
    hasher.update(encoded);
    Ok(GeminiSelectedContent::Structured {
        value: serde_json::from_slice(encoded)
            .map_err(|error| format!("invalid Gemini result value: {error}"))?,
        sha256: hasher.finalize().into(),
    })
}

pub(super) fn result_fallback_identity_sha256(
    call_id: Option<&str>,
    tool_name: Option<&str>,
    outcome: &OutputOutcomeMetadata,
    content_kind: &[u8],
    content_sha256: Option<&[u8; 32]>,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(RESULT_FALLBACK_ID_DOMAIN);
    hash_page_optional_text(&mut hasher, call_id);
    hash_page_optional_text(&mut hasher, tool_name);
    hasher.update([match outcome.outcome {
        OutputOutcome::Success => 0,
        OutputOutcome::Failure => 1,
        OutputOutcome::Timeout => 2,
        OutputOutcome::Unknown => 3,
    }]);
    hash_page_optional_i64(&mut hasher, outcome.exit_code.map(i64::from));
    hash_page_optional_u64(&mut hasher, outcome.duration_ms);
    hash_page_bytes(&mut hasher, content_kind);
    if let Some(content_sha256) = content_sha256 {
        hasher.update(content_sha256);
    }
    hasher.finalize().into()
}

pub(super) fn result_event_identity(
    native_record_id: Option<&str>,
    output: &ProbedGeminiOutput,
    output_index: usize,
) -> GeminiEventIdentity {
    let fallback = hex_sha256(output.fallback_identity_sha256);
    let identity = if let Some(native_record_id) = native_record_id {
        let subrecord = output.call_id.as_deref().map_or_else(
            || format!("fallback-sha256:{fallback}"),
            |call_id| format!("call:{}:{call_id}", call_id.len()),
        );
        format!(
            "gemini-result-v2:record:{}:{native_record_id}:subrecord:{subrecord}:index:{output_index}",
            native_record_id.len()
        )
    } else {
        format!("gemini-result-v2:fallback-sha256:{fallback}:index:{output_index}")
    };
    GeminiEventIdentity::NativeRecordId(identity)
}

pub(super) fn hex_sha256(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

impl GeminiRawJson<'_> {
    fn strict_optional_string(&mut self) -> std::result::Result<Option<String>, String> {
        self.whitespace();
        match self.peek() {
            Some(b'"') => self
                .string(MAX_GEMINI_REPOSITORY_STRING_CHARS)?
                .exact()
                .ok_or_else(|| "Gemini result metadata string exceeded the bound".to_owned())
                .map(Some),
            Some(b'n') => {
                self.consume_literal(b"null")?;
                Ok(None)
            }
            _ => Err(format!(
                "Gemini result metadata field is not a string near byte {}",
                self.offset
            )),
        }
    }

    fn repository_args(
        &mut self,
        depth: usize,
    ) -> std::result::Result<GeminiRepositoryArgs, String> {
        self.whitespace();
        if self.peek() != Some(b'{') {
            self.skip_value(depth)?;
            return Ok(GeminiRepositoryArgs {
                ambiguous_native_fields: true,
                ..GeminiRepositoryArgs::default()
            });
        }
        self.take(b'{')?;
        self.whitespace();
        let mut args = GeminiRepositoryArgs::default();
        let mut command_seen = false;
        let mut workdir_seen = false;
        if self.peek() == Some(b'}') {
            self.take(b'}')?;
            return Ok(args);
        }
        loop {
            let key = self.key()?;
            self.whitespace();
            self.take(b':')?;
            self.whitespace();
            match key.as_deref() {
                Some("command") => {
                    let (value, command_too_large) = self.repository_command(depth)?;
                    args.ambiguous_native_fields |=
                        command_seen || (value.is_none() && !command_too_large);
                    args.command_too_large |= command_too_large;
                    if command_seen {
                        args.command = None;
                    } else {
                        args.command = value;
                    }
                    command_seen = true;
                }
                Some("dir_path") => {
                    let value = self.repository_string(depth)?;
                    args.ambiguous_native_fields |= workdir_seen || value.is_none();
                    if workdir_seen {
                        args.declared_workdir = None;
                    } else {
                        args.declared_workdir = value;
                    }
                    workdir_seen = true;
                }
                Some("path" | "file_path" | "filePath") => {
                    let value = self.repository_string(depth)?;
                    if let Some(path) = value {
                        if args.file_paths.len() < MAX_GEMINI_REPOSITORY_PATHS {
                            args.file_paths.push(path);
                        } else {
                            args.ambiguous_native_fields = true;
                        }
                    } else {
                        args.ambiguous_native_fields = true;
                    }
                }
                _ => self.skip_value(depth)?,
            }
            self.whitespace();
            match self.peek() {
                Some(b',') => {
                    self.take(b',')?;
                    self.whitespace();
                }
                Some(b'}') => {
                    self.take(b'}')?;
                    break;
                }
                _ => {
                    return Err(format!(
                        "invalid Gemini tool args near byte {}",
                        self.offset
                    ));
                }
            }
        }
        Ok(args)
    }

    fn repository_string(&mut self, depth: usize) -> std::result::Result<Option<String>, String> {
        self.whitespace();
        if self.peek() == Some(b'"') {
            return Ok(self.string(MAX_GEMINI_REPOSITORY_STRING_CHARS)?.exact());
        }
        self.skip_value(depth)?;
        Ok(None)
    }

    fn repository_command(
        &mut self,
        depth: usize,
    ) -> std::result::Result<(Option<String>, bool), String> {
        self.whitespace();
        if self.peek() == Some(b'"') {
            return Ok(self
                .string(crate::repository_attribution::MAX_COMMAND_BYTES)?
                .bounded_command());
        }
        self.skip_value(depth)?;
        Ok((None, false))
    }

    fn result_calls(
        &mut self,
        depth: usize,
    ) -> std::result::Result<(Vec<GeminiRawResultCall>, bool), String> {
        self.whitespace();
        self.take(b'[')?;
        self.whitespace();
        let mut calls = Vec::new();
        let mut unknown_member = false;
        if self.peek() == Some(b']') {
            self.take(b']')?;
            return Ok((calls, false));
        }
        loop {
            let (call, unknown) = self.result_call(depth.saturating_add(1))?;
            unknown_member |= unknown;
            if let Some(call) = call {
                unknown_member |= call.result.is_none();
                calls.push(call);
            }
            self.whitespace();
            match self.peek() {
                Some(b',') => {
                    self.take(b',')?;
                    self.whitespace();
                }
                Some(b']') => {
                    self.take(b']')?;
                    break;
                }
                _ => {
                    return Err(format!(
                        "invalid Gemini toolCalls array near byte {}",
                        self.offset
                    ));
                }
            }
        }
        Ok((calls, unknown_member))
    }

    fn result_call(
        &mut self,
        depth: usize,
    ) -> std::result::Result<(Option<GeminiRawResultCall>, bool), String> {
        self.whitespace();
        if self.peek() != Some(b'{') {
            self.skip_value(depth)?;
            return Ok((None, true));
        }
        self.take(b'{')?;
        self.whitespace();
        let mut call = GeminiRawResultCall {
            id: None,
            name: None,
            args: GeminiRepositoryArgs::default(),
            result: None,
            outcome: GeminiOutputOutcomeDto::default(),
        };
        let mut id_seen = false;
        let mut name_seen = false;
        let mut args_seen = false;
        let mut result_seen = false;
        if self.peek() == Some(b'}') {
            self.take(b'}')?;
            return Ok((Some(call), true));
        }
        loop {
            let key = self.key()?;
            self.whitespace();
            self.take(b':')?;
            self.whitespace();
            match key.as_deref() {
                Some("id") => {
                    let value = self.optional_string()?;
                    call.args.ambiguous_native_fields |= id_seen || value.is_none();
                    if id_seen {
                        call.id = None;
                    } else {
                        call.id = value;
                    }
                    id_seen = true;
                }
                Some("name") => {
                    let value = self.optional_string()?;
                    call.args.ambiguous_native_fields |= name_seen || value.is_none();
                    if name_seen {
                        call.name = None;
                    } else {
                        call.name = value;
                    }
                    name_seen = true;
                }
                Some("args") => {
                    let mut args = self.repository_args(depth.saturating_add(1))?;
                    args.ambiguous_native_fields |= args_seen || call.args.ambiguous_native_fields;
                    if args_seen {
                        args.command = None;
                        args.declared_workdir = None;
                        args.file_paths.clear();
                    }
                    args_seen = true;
                    call.args = args;
                }
                Some("result") => {
                    let result = self.output_value(depth.saturating_add(1))?;
                    call.args.ambiguous_native_fields |= result_seen;
                    result_seen = true;
                    call.result = Some(result);
                }
                Some(key) => {
                    if !self.outcome_field(key, &mut call.outcome, depth)? {
                        call.outcome.framing_unknown = true;
                        self.skip_value(depth)?;
                    }
                }
                None => {
                    call.outcome.framing_unknown = true;
                    self.skip_value(depth)?;
                }
            }
            self.whitespace();
            match self.peek() {
                Some(b',') => {
                    self.take(b',')?;
                    self.whitespace();
                }
                Some(b'}') => {
                    self.take(b'}')?;
                    break;
                }
                _ => {
                    return Err(format!(
                        "invalid Gemini result tool call near byte {}",
                        self.offset
                    ));
                }
            }
        }
        Ok((Some(call), false))
    }
}

fn has_structural_diagnostic(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(has_structural_diagnostic),
        Value::Object(values) => values.iter().any(|(key, value)| {
            matches!(
                key.as_str(),
                "stderr"
                    | "warning"
                    | "warnings"
                    | "error"
                    | "errors"
                    | "diagnostic"
                    | "diagnostics"
            ) || has_structural_diagnostic(value)
        }),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}
