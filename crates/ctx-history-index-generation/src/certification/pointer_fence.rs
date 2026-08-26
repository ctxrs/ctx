use std::io::Read as _;

use super::*;

struct PointerFileSnapshot {
    file: File,
    identity: FileIdentity,
}

/// Exact durable predecessor authority retained from writer admission through
/// candidate activation. An incompatible pointer remains opaque but is still
/// fenced by its native control-file identity.
pub struct ActiveGenerationPointerFence {
    expected: Option<PointerFileSnapshot>,
    topology_authority: Option<ActiveGenerationPointer>,
}

impl ActiveGenerationPointerFence {
    /// Captures the current pointer without decoding an unsupported version.
    /// A present pointer may be opaque only when the normal loader independently
    /// classifies that exact native file as version-incompatible.
    #[doc(hidden)]
    pub fn capture(
        root: &Path,
        topology_authority: Option<&ActiveGenerationPointer>,
    ) -> Result<Self> {
        ensure_real_directory(root)?;
        let path = root.join(crate::ACTIVE_GENERATION_POINTER_FILE);
        let expected = match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
            Ok(_) => {
                let (mut file, identity) = open_regular_file(&path)?;
                let length =
                    usize::try_from(identity.length()).map_err(|_| IndexError::CountOverflow)?;
                let mut bytes = Vec::with_capacity(length);
                file.read_to_end(&mut bytes)?;
                if bytes.len() != length {
                    return Err(IndexError::ConcurrentGenerationChange);
                }
                if let Some(pointer) = topology_authority {
                    if serde_json::to_vec(pointer)? != bytes {
                        return Err(IndexError::ConcurrentGenerationChange);
                    }
                } else if !matches!(
                    load_active_generation_pointer(root),
                    Err(IndexError::UnsupportedActiveGenerationPointer(_))
                ) {
                    return Err(IndexError::ConcurrentGenerationChange);
                }
                Some(PointerFileSnapshot { file, identity })
            }
        };
        if topology_authority.is_some() && expected.is_none() {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        let fence = Self {
            expected,
            topology_authority: topology_authority.cloned(),
        };
        fence.validate(root)?;
        Ok(fence)
    }

    pub(crate) fn topology_authority(&self) -> Option<&ActiveGenerationPointer> {
        self.topology_authority.as_ref()
    }

    pub fn validate(&self, root: &Path) -> Result<()> {
        let path = root.join(crate::ACTIVE_GENERATION_POINTER_FILE);
        let Some(expected) = &self.expected else {
            return match fs::symlink_metadata(path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                _ => Err(IndexError::ConcurrentGenerationChange),
            };
        };
        if file_identity(&expected.file).map_err(|_| IndexError::ConcurrentGenerationChange)?
            != expected.identity
        {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        let (_, current) =
            open_regular_file(&path).map_err(|_| IndexError::ConcurrentGenerationChange)?;
        if current != expected.identity {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        Ok(())
    }
}
