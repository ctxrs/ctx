use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[repr(transparent)]
        pub struct $name(pub Uuid);

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                let value = self.0.to_string();
                serializer.serialize_str(&value)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                let uuid = Uuid::parse_str(&value).map_err(serde::de::Error::custom)?;
                Ok(Self(uuid))
            }
        }

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl From<Uuid> for $name {
            fn from(value: Uuid) -> Self {
                Self(value)
            }
        }

        impl From<$name> for Uuid {
            fn from(value: $name) -> Self {
                value.0
            }
        }
    };
}

id_type!(WorkspaceId);
id_type!(AccountId);
id_type!(OrgId);
id_type!(OrgMembershipId);
id_type!(DaemonEnrollmentId);
id_type!(OrgPolicySnapshotId);
id_type!(RunGrantId);
id_type!(PolicyDecisionEventId);
id_type!(SandboxInstanceId);
id_type!(TaskId);
id_type!(WorktreeId);
id_type!(SessionId);
id_type!(MessageId);
id_type!(SessionEventId);
id_type!(ArtifactId);
id_type!(CheckId);
id_type!(RunId);
id_type!(TurnId);
id_type!(ConnectionProfileId);
id_type!(MobileDeviceId);
id_type!(WorkspaceAttachmentId);
id_type!(TerminalId);
id_type!(MergeQueueEntryId);
id_type!(MergeQueueRunId);

macro_rules! string_id_type {
    ($name:ident, $prefix:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new() -> Self {
                Self(format!("{}{}", $prefix, Uuid::new_v4().simple()))
            }

            pub fn from_id(id: impl Into<String>) -> Self {
                Self(id.into())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_string())
            }
        }
    };
}

string_id_type!(ChangeSetId, "chg_");
string_id_type!(ContributionId, "con_");
string_id_type!(AgentWorkSourceRecordId, "rec_");
