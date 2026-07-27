//! Bounded, read-only recovery of complete message bodies from provider sources.
//!
//! The Store and CLI own selection and policy. Provider-family implementations
//! own native source parsing. Complete-message batches are all-or-nothing;
//! result-content batches may omit individually unverifiable records after the
//! shared source itself has been opened and frozen successfully.

pub mod jsonl;
mod locator;
mod registry;
mod resolver;
pub mod source_access;
pub mod sqlite;
pub mod structured;

pub use locator::{
    attach_verified_content_locator, CompleteContentBodyDigest, CompleteContentHashAuthority,
    CompleteContentSourceFamily, CompleteContentSourceLocator, SourceSnapshot,
    VerifiedContentLocatorV1, VerifiedContentLocatorsV1, VerifiedContentRole,
    COMPLETE_CONTENT_INDEXED_MESSAGE_LIMIT_CHARS, COMPLETE_CONTENT_MAX_BODY_BYTES,
    COMPLETE_CONTENT_MAX_LOCATOR_BYTES, VERIFIED_CONTENT_LOCATORS_MAX_BYTES,
    VERIFIED_CONTENT_LOCATORS_MAX_ENTRIES, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
};
pub use registry::{
    verified_content_address_supported, verified_content_profile,
    verified_content_profile_for_locator, verified_content_profile_matches,
    verified_content_route_matches, verified_content_route_supported, VerifiedContentContract,
    VerifiedContentPlatform, VerifiedContentPlatformDisposition, VerifiedContentRoute,
    VerifiedContentRouteStatus, VERIFIED_CONTENT_RELEASE_PLATFORMS, VERIFIED_CONTENT_ROUTES,
};
pub use resolver::{
    CompleteContentError, CompleteContentErrorKind, CompleteContentResolver,
    CompleteContentResolverRegistry, CompleteMessage, CompleteMessageRequest,
    ResolvedResultContent, ResultContentRequest, ResultContentResolver,
    ResultContentResolverRegistry, SourceVerification,
};
pub use source_access::{AuthorizedSourceRoute, BrokeredSourceAccess, SourceAccessBroker};

#[cfg(test)]
mod tests;
