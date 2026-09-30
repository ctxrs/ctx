use crate::hosted::{HostedCompletion, HostedOperation};
use ctx_client_observability::analytics::*;

pub(super) fn project(fact: HostedCompletion) -> PublicEventV1 {
    let operation = match fact.operation {
        HostedOperation::Existing(v) => v,
        HostedOperation::ArchiveVerify => HostedOperationV1::ArchiveVerify,
        HostedOperation::ServerCollectionCreate => HostedOperationV1::ServerCollectionCreate,
        HostedOperation::ServerUserCreate => HostedOperationV1::ServerUserCreate,
        HostedOperation::ServerUserList => HostedOperationV1::ServerUserList,
        HostedOperation::ServerUserCredentials => HostedOperationV1::ServerUserCredentials,
        HostedOperation::ServerUserCredential => HostedOperationV1::ServerUserCredential,
        HostedOperation::ServerPublications => HostedOperationV1::ServerPublications,
        HostedOperation::ServerStatus => HostedOperationV1::ServerStatus,
        HostedOperation::RemotePause => HostedOperationV1::RemotePause,
        HostedOperation::RemoteResume => HostedOperationV1::RemoteResume,
        HostedOperation::RemoteStatus => HostedOperationV1::RemoteStatus,
        HostedOperation::RemoteRemove => HostedOperationV1::RemoteRemove,
    };
    let delivery = match fact.result {
        Ok(()) => DeliveryEvidence::KnownComplete,
        Err(HostedFailureV1::Output) => DeliveryEvidence::Failed,
        // An operation error does not prove that no partial output preceded it.
        Err(HostedFailureV1::Operation(_)) => DeliveryEvidence::Unknown,
    };
    hosted_operation_completed_with_measurements(
        operation,
        fact.output,
        fact.result,
        fact.duration,
        HostedMeasurements {
            elapsed: fact.duration,
            timings: ProductTimings::default(),
            delivery,
            result: None,
        },
    )
}
