use std::{borrow::Cow, ffi::OsStr};

use ctx_history_capture_model::{
    ProviderRouteRole, ProviderRouteRoleError, ProviderSourceRouteProvenance,
};
use sha2::{Digest, Sha256};

const NATIVE_OS_STR_ID_DIGEST_DOMAIN: &[u8] = b"ctx.provider-route-native-os-str-id.v1\0";
const NATIVE_ID_INLINE_COMPONENT: &[u8] = b"native-id";
const NATIVE_ID_DIGEST_COMPONENT: &[u8] = b"native-id-sha256";
const UTF8_TAG: &[u8] = b"utf8";
#[cfg(unix)]
const UNIX_BYTES_TAG: &[u8] = b"unix-bytes";
#[cfg(windows)]
const WINDOWS_UTF16LE_TAG: &[u8] = b"windows-utf16le";

pub(super) const AUTOMATIC_ROUTE_ROLE_UNAVAILABLE_REASON: &str =
    "the provider's stable automatic route role exceeds discovery limits; use an exact --path";

pub(super) fn automatic_route_provenance<I, B>(
    components: I,
) -> Result<ProviderSourceRouteProvenance, ProviderRouteRoleError>
where
    I: IntoIterator<Item = B>,
    B: AsRef<[u8]>,
{
    ProviderRouteRole::from_dynamic(components)
        .map(|route_role| ProviderSourceRouteProvenance::Automatic { route_role })
}

/// Persists a provider-native `OsStr` identifier without relying on Rust's
/// version-specific internal `OsStr` encoding.
///
/// Valid Unicode is tagged UTF-8 on every platform. Invalid Unix strings are
/// tagged raw bytes, while invalid Windows strings are tagged UTF-16LE code
/// units. Route-role component framing keeps tags and payloads collision-safe.
/// Oversized payloads hash the same tagged, length-framed representation and
/// retain the tag as a role component.
pub(super) fn automatic_route_provenance_with_native_os_str_id(
    prefix: &[&[u8]],
    native_id: &OsStr,
    suffix: &[&[u8]],
) -> Result<ProviderSourceRouteProvenance, ProviderRouteRoleError> {
    let encoded = persisted_os_str_encoding(native_id)?;
    let components = native_id_role_components(
        prefix,
        NATIVE_ID_INLINE_COMPONENT,
        encoded.tag,
        encoded.payload.as_ref(),
        suffix,
    );
    match automatic_route_provenance(components) {
        Ok(route_provenance) => Ok(route_provenance),
        Err(_) => {
            let mut digest = Sha256::new();
            digest.update(NATIVE_OS_STR_ID_DIGEST_DOMAIN);
            update_digest_frame(&mut digest, encoded.tag)?;
            update_digest_frame(&mut digest, encoded.payload.as_ref())?;
            let digest = digest.finalize();
            let components = native_id_role_components(
                prefix,
                NATIVE_ID_DIGEST_COMPONENT,
                encoded.tag,
                digest.as_ref(),
                suffix,
            );
            automatic_route_provenance(components)
        }
    }
}

struct PersistedOsStrEncoding<'a> {
    tag: &'static [u8],
    payload: Cow<'a, [u8]>,
}

fn persisted_os_str_encoding(
    native_id: &OsStr,
) -> Result<PersistedOsStrEncoding<'_>, ProviderRouteRoleError> {
    if let Some(value) = native_id.to_str() {
        return Ok(PersistedOsStrEncoding {
            tag: UTF8_TAG,
            payload: Cow::Borrowed(value.as_bytes()),
        });
    }

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        Ok(PersistedOsStrEncoding {
            tag: UNIX_BYTES_TAG,
            payload: Cow::Borrowed(native_id.as_bytes()),
        })
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        let payload = native_id.encode_wide().flat_map(u16::to_le_bytes).collect();
        Ok(PersistedOsStrEncoding {
            tag: WINDOWS_UTF16LE_TAG,
            payload: Cow::Owned(payload),
        })
    }

    #[cfg(not(any(unix, windows)))]
    Err(ProviderRouteRoleError)
}

fn native_id_role_components(
    prefix: &[&[u8]],
    form: &[u8],
    tag: &[u8],
    payload: &[u8],
    suffix: &[&[u8]],
) -> Vec<Vec<u8>> {
    let mut components = Vec::with_capacity(prefix.len().saturating_add(suffix.len() + 3));
    components.extend(prefix.iter().map(|component| component.to_vec()));
    components.push(form.to_vec());
    components.push(tag.to_vec());
    components.push(payload.to_vec());
    components.extend(suffix.iter().map(|component| component.to_vec()));
    components
}

fn update_digest_frame(digest: &mut Sha256, value: &[u8]) -> Result<(), ProviderRouteRoleError> {
    let length = u64::try_from(value.len()).map_err(|_| ProviderRouteRoleError)?;
    digest.update(length.to_be_bytes());
    digest.update(value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use ctx_history_capture_model::MAX_PROVIDER_ROUTE_ROLE_BYTES;

    use super::*;

    fn route_role(provenance: &ProviderSourceRouteProvenance) -> &ProviderRouteRole {
        provenance
            .automatic_route_role()
            .expect("test provenance should carry an automatic role")
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn automatic_role_bytes_are_exactly_length_framed_and_collision_safe() {
        let role = automatic_route_provenance([b"agent".as_slice(), b"main".as_slice()])
            .expect("bounded role");
        assert_eq!(
            hex(route_role(&role).as_bytes()),
            "0000000000000000056167656e7400000000000000046d61696e"
        );

        let split =
            automatic_route_provenance([b"a".as_slice(), b"bc".as_slice()]).expect("bounded role");
        let joined =
            automatic_route_provenance([b"ab".as_slice(), b"c".as_slice()]).expect("bounded role");
        assert_ne!(route_role(&split), route_role(&joined));
    }

    #[test]
    fn unicode_os_str_ids_have_an_exact_tagged_utf8_encoding() {
        let role = automatic_route_provenance_with_native_os_str_id(
            &[b"installation", b"profile"],
            OsStr::new("café-雪"),
            &[b"stable"],
        )
        .expect("Unicode native id should be bounded");

        assert_eq!(
            hex(route_role(&role).as_bytes()),
            "00000000000000000c696e7374616c6c6174696f6e000000000000000770726f66696c6500000000000000096e61746976652d69640000000000000004757466380000000000000009636166c3a92de99baa0000000000000006737461626c65"
        );
    }

    #[cfg(unix)]
    #[test]
    fn invalid_unix_os_str_ids_have_an_exact_tagged_raw_byte_encoding() {
        use std::os::unix::ffi::OsStrExt;

        let role = automatic_route_provenance_with_native_os_str_id(
            &[b"installation", b"profile"],
            OsStr::from_bytes(b"profile-\xff"),
            &[b"stable"],
        )
        .expect("invalid UTF-8 Unix native id should be bounded");

        assert_eq!(
            hex(route_role(&role).as_bytes()),
            "00000000000000000c696e7374616c6c6174696f6e000000000000000770726f66696c6500000000000000096e61746976652d6964000000000000000a756e69782d6279746573000000000000000970726f66696c652dff0000000000000006737461626c65"
        );
    }

    #[cfg(windows)]
    #[test]
    fn unpaired_windows_surrogate_has_an_exact_tagged_utf16le_encoding() {
        use std::{ffi::OsString, os::windows::ffi::OsStringExt};

        let native_id = OsString::from_wide(&[0x0070, 0xd800, 0x0061]);
        let role = automatic_route_provenance_with_native_os_str_id(
            &[b"installation", b"profile"],
            &native_id,
            &[b"stable"],
        )
        .expect("unpaired-surrogate Windows native id should be bounded");

        assert_eq!(
            hex(route_role(&role).as_bytes()),
            "00000000000000000c696e7374616c6c6174696f6e000000000000000770726f66696c6500000000000000096e61746976652d6964000000000000000f77696e646f77732d75746631366c650000000000000006700000d861000000000000000006737461626c65"
        );
    }

    #[test]
    fn oversized_os_str_ids_have_an_exact_tagged_digest_encoding() {
        let oversized = "x".repeat(MAX_PROVIDER_ROUTE_ROLE_BYTES);
        let first = automatic_route_provenance_with_native_os_str_id(
            &[b"installation", b"profile"],
            OsStr::new(&oversized),
            &[b"stable"],
        )
        .expect("oversized native id should use its bounded digest form");
        let repeated = automatic_route_provenance_with_native_os_str_id(
            &[b"installation", b"profile"],
            OsStr::new(&oversized),
            &[b"stable"],
        )
        .expect("same oversized native id should remain valid");
        let distinct_id = "y".repeat(MAX_PROVIDER_ROUTE_ROLE_BYTES);
        let distinct = automatic_route_provenance_with_native_os_str_id(
            &[b"installation", b"profile"],
            OsStr::new(&distinct_id),
            &[b"stable"],
        )
        .expect("distinct oversized native id should remain valid");

        assert_eq!(first, repeated);
        assert_ne!(first, distinct);
        assert_eq!(
            hex(route_role(&first).as_bytes()),
            "00000000000000000c696e7374616c6c6174696f6e000000000000000770726f66696c6500000000000000106e61746976652d69642d73686132353600000000000000047574663800000000000000209297d6b0c16ad5407f4b35b6e0a0ceba1b7048c19a36e5ca7a6ab9748c7a4dff0000000000000006737461626c65"
        );
    }
}
