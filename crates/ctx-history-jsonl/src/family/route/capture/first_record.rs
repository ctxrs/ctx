use super::*;

/// Applies the shared physical JSONL contract before any provider semantic
/// classification. A nonempty first record without its framing terminator is
/// pending/incomplete; zero-byte files and complete malformed records retain
/// their existing provider semantics.
pub(in crate::family::route) fn classify_incomplete_first_records<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    opening: &mut JsonlFamilyInventory<JsonlRuntimeError<R>>,
) -> JsonlResult<(), JsonlRuntimeError<R>> {
    if opening.root_missing {
        return Ok(());
    }
    let mut changed = false;
    let mut classified = Vec::with_capacity(opening.members.len());
    for member in std::mem::take(&mut opening.members) {
        match member {
            JsonlFamilyInventoryMember::Accepted { identity, leaf } if !leaf.whole_record => {
                let (incomplete, frozen_prefix_sha256) = first_record_is_incomplete(
                    &leaf.source_path,
                    &leaf.authority,
                    &leaf.authority_path,
                    &leaf.observation,
                    FirstRecordProbe {
                        encoding: adapter.physical_encoding(&leaf),
                        framing: adapter.record_framing(),
                        freeze_observation_at_scan: leaf.frozen_scan_observation().is_some(),
                        admitted_length: leaf.admitted_length(),
                    },
                )?;
                if incomplete {
                    changed = true;
                    let mut pending = JsonlFamilyPendingLeaf::bind_observed(
                        leaf.source_path,
                        leaf.authority_path,
                        leaf.observation,
                        leaf.binding,
                        Some(leaf.source),
                    );
                    pending.frozen_prefix_sha256 = frozen_prefix_sha256;
                    classified.push(JsonlFamilyInventoryMember::Pending {
                        identity,
                        leaf: pending,
                    });
                } else {
                    classified.push(JsonlFamilyInventoryMember::Accepted { identity, leaf });
                }
            }
            JsonlFamilyInventoryMember::Quarantined { identity, leaf } => {
                // Provider classification wins once a member owns a diagnosed
                // source failure; a framing probe must not erase that outcome.
                let incomplete = if leaf.logical_source_failure.is_some() {
                    false
                } else if let Some(observation) = &leaf.observation {
                    let authority = exact_member_authority(
                        &opening.authorities,
                        &leaf.source_path,
                        &leaf.authority_path,
                    )?;
                    first_record_is_incomplete(
                        &leaf.source_path,
                        authority,
                        &leaf.authority_path,
                        observation,
                        FirstRecordProbe {
                            encoding: leaf.physical_encoding,
                            framing: adapter.record_framing(),
                            freeze_observation_at_scan: false,
                            admitted_length: observation.length(),
                        },
                    )?
                    .0
                } else {
                    false
                };
                if incomplete {
                    changed = true;
                    classified.push(JsonlFamilyInventoryMember::Pending {
                        identity,
                        leaf: JsonlFamilyPendingLeaf::bind_observed(
                            leaf.source_path,
                            leaf.authority_path,
                            leaf.observation
                                .expect("incomplete quarantined leaf has an admitted observation"),
                            leaf.proof,
                            leaf.quarantined_source,
                        ),
                    });
                } else {
                    classified.push(JsonlFamilyInventoryMember::Quarantined { identity, leaf });
                }
            }
            member => classified.push(member),
        }
    }
    opening.members = classified;
    if changed {
        opening.rebuild_observation()?;
    }
    Ok(())
}

struct FirstRecordProbe {
    encoding: JsonlPhysicalEncoding,
    framing: JsonlRecordFraming,
    freeze_observation_at_scan: bool,
    admitted_length: u64,
}

fn first_record_is_incomplete<E: JsonlFamilyError>(
    source_path: &Path,
    authority: &Arc<ProviderSourceRoot<E>>,
    authority_path: &Path,
    expected: &JsonlFileObservation,
    probe: FirstRecordProbe,
) -> JsonlResult<(bool, Option<[u8; 32]>), E> {
    let FirstRecordProbe {
        encoding,
        framing,
        freeze_observation_at_scan,
        admitted_length,
    } = probe;
    let opened = authority.open_file(authority_path)?;
    let current = if freeze_observation_at_scan {
        observe_opened_file_allow_append(source_path, &opened)?
    } else {
        observe_opened_file(source_path, &opened)?
    };
    // Frozen-scan leaves already bind publication to the discovery-time
    // observation. Classify their first record inside that bound while later
    // scan and terminal proofs authenticate the prefix; newly appended bytes
    // belong to the next refresh. Exact leaves retain exact preflight behavior.
    if (freeze_observation_at_scan && !expected.admits_frozen_prefix_in(&current))
        || (!freeze_observation_at_scan && expected != &current)
    {
        return Err(E::source_changed());
    }
    let observation = expected;
    if admitted_length > observation.length() {
        return Err(E::source_changed());
    }
    if admitted_length == 0 {
        opened.revalidate_same_object()?;
        return Ok((false, None));
    }
    let mut file = opened.reopen_same_object()?;
    if encoding == JsonlPhysicalEncoding::RawJsonl && admitted_length >= 4 {
        let mut magic = [0_u8; 4];
        file.read_exact(&mut magic)?;
        file.seek(SeekFrom::Start(0))?;
        if magic == [0x28, 0xb5, 0x2f, 0xfd] {
            // A quarantined member does not necessarily have a provider leaf
            // from which to ask for encoding. Never reinterpret an ordinary
            // Zstandard stream as an unfinished raw JSONL record.
            opened.revalidate_same_object()?;
            return Ok((false, None));
        }
    }
    let mut stream = JsonlPhysicalStream::open_with_encoding(
        file,
        admitted_length,
        0,
        0,
        encoding,
        framing,
        JsonlPhysicalDigest::full_and_complete(
            JsonlResumableSha256::new(),
            JsonlResumableSha256::new(),
        ),
        E::source_changed,
    )?;
    let incomplete = stream.next_record()?.is_none_or(|record| !record.complete);
    opened.revalidate_same_object()?;
    let digest = if incomplete && freeze_observation_at_scan {
        stream
            .digest()
            .full_hasher()
            .map(JsonlResumableSha256::digest)
    } else {
        None
    };
    Ok((incomplete, digest))
}
