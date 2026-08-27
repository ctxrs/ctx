use super::super::*;

impl<E: JsonlFamilyError> JsonlReader<E> {
    #[cfg(any(test, feature = "test-support"))]
    pub fn open(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile<E>>,
        previous: Option<&JsonlCheckpoint>,
        probe: Option<JsonlProbe>,
    ) -> JsonlResult<Self, E> {
        Self::open_with_record_framing(
            identity,
            source_file,
            previous,
            probe,
            JsonlRecordFraming::ordinary(),
        )
    }

    pub fn open_with_record_framing(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile<E>>,
        previous: Option<&JsonlCheckpoint>,
        probe: Option<JsonlProbe>,
        record_framing: JsonlRecordFraming,
    ) -> JsonlResult<Self, E> {
        Self::open_with_record_framing_and_encoding(
            identity,
            source_file,
            previous,
            probe,
            JsonlPhysicalEncoding::RawJsonl,
            record_framing,
        )
    }

    pub fn open_with_record_framing_and_encoding(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile<E>>,
        previous: Option<&JsonlCheckpoint>,
        probe: Option<JsonlProbe>,
        physical_encoding: JsonlPhysicalEncoding,
        record_framing: JsonlRecordFraming,
    ) -> JsonlResult<Self, E> {
        Self::open_with_framing(
            identity,
            source_file,
            previous,
            probe,
            JsonlReaderFramingOptions {
                physical_encoding,
                record_framing,
                whole_record: false,
                bind_admitted_eof: false,
                logical_eof: None,
                deferred_append_eof_sha256: None,
                frozen_observation: None,
                direct_append: false,
                route_resources: None,
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn open_with_record_framing_and_encoding_and_resources(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile<E>>,
        previous: Option<&JsonlCheckpoint>,
        probe: Option<JsonlProbe>,
        physical_encoding: JsonlPhysicalEncoding,
        record_framing: JsonlRecordFraming,
        logical_eof: Option<u64>,
        route_resources: &ctx_history_capture_runtime::SourceBackedRouteResources,
    ) -> JsonlResult<Self, E> {
        Self::open_with_framing(
            identity,
            source_file,
            previous,
            probe,
            JsonlReaderFramingOptions {
                physical_encoding,
                record_framing,
                whole_record: false,
                bind_admitted_eof: logical_eof.is_some(),
                logical_eof,
                deferred_append_eof_sha256: None,
                frozen_observation: None,
                direct_append: false,
                route_resources: Some(route_resources),
            },
        )
    }

    pub fn open_semantic_with_record_framing(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile<E>>,
        previous: Option<&JsonlCheckpoint>,
        mode: JsonlSemanticPreflightMode,
        probe: Option<JsonlProbe>,
        record_framing: JsonlRecordFraming,
        frozen_observation: Option<&JsonlFileObservation>,
    ) -> JsonlResult<Self, E> {
        Self::open_semantic_with_record_framing_and_encoding(
            identity,
            source_file,
            previous,
            mode,
            probe,
            JsonlPhysicalEncoding::RawJsonl,
            record_framing,
            frozen_observation,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn open_semantic_with_record_framing_and_encoding(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile<E>>,
        previous: Option<&JsonlCheckpoint>,
        mode: JsonlSemanticPreflightMode,
        probe: Option<JsonlProbe>,
        physical_encoding: JsonlPhysicalEncoding,
        record_framing: JsonlRecordFraming,
        frozen_observation: Option<&JsonlFileObservation>,
    ) -> JsonlResult<Self, E> {
        Self::open_semantic_with_record_framing_and_encoding_direct(
            identity,
            source_file,
            previous,
            mode,
            probe,
            physical_encoding,
            record_framing,
            frozen_observation,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn open_semantic_with_record_framing_and_encoding_direct(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile<E>>,
        previous: Option<&JsonlCheckpoint>,
        mode: JsonlSemanticPreflightMode,
        probe: Option<JsonlProbe>,
        physical_encoding: JsonlPhysicalEncoding,
        record_framing: JsonlRecordFraming,
        frozen_observation: Option<&JsonlFileObservation>,
        direct_append: bool,
    ) -> JsonlResult<Self, E> {
        Self::open_semantic_with_record_framing_and_encoding_direct_and_resources(
            identity,
            source_file,
            previous,
            mode,
            probe,
            physical_encoding,
            record_framing,
            frozen_observation,
            direct_append,
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn open_semantic_with_record_framing_and_encoding_direct_and_resources(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile<E>>,
        previous: Option<&JsonlCheckpoint>,
        mode: JsonlSemanticPreflightMode,
        probe: Option<JsonlProbe>,
        physical_encoding: JsonlPhysicalEncoding,
        record_framing: JsonlRecordFraming,
        frozen_observation: Option<&JsonlFileObservation>,
        direct_append: bool,
        logical_eof: Option<u64>,
        route_resources: Option<&ctx_history_capture_runtime::SourceBackedRouteResources>,
    ) -> JsonlResult<Self, E> {
        let (bind_admitted_eof, deferred_append_eof_sha256) = match mode {
            JsonlSemanticPreflightMode::AdmittedEof(previous) => (true, previous.map(Some)),
            JsonlSemanticPreflightMode::CompletePrefix => (false, Some(None)),
        };
        Self::open_with_framing(
            identity,
            source_file,
            previous,
            probe,
            JsonlReaderFramingOptions {
                physical_encoding,
                record_framing,
                whole_record: false,
                bind_admitted_eof: bind_admitted_eof || logical_eof.is_some(),
                logical_eof,
                deferred_append_eof_sha256,
                frozen_observation,
                direct_append,
                route_resources,
            },
        )
    }

    pub fn open_whole_record(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile<E>>,
        previous: Option<&JsonlCheckpoint>,
    ) -> JsonlResult<Self, E> {
        Self::open_with_framing(
            identity,
            source_file,
            previous,
            None,
            JsonlReaderFramingOptions {
                physical_encoding: JsonlPhysicalEncoding::RawJsonl,
                record_framing: JsonlRecordFraming::ordinary(),
                whole_record: true,
                bind_admitted_eof: false,
                logical_eof: None,
                deferred_append_eof_sha256: None,
                frozen_observation: None,
                direct_append: false,
                route_resources: None,
            },
        )
    }
}
