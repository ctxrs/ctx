use std::{
    ffi::{OsStr, OsString},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use crate::{
    environment::CompanionEnvironment,
    limits::LimitConfiguration,
    protocol::{
        PROTOCOL_CLI_COMMAND, PROTOCOL_ENTRYPOINT_ARGUMENT, PROTOCOL_HANDSHAKE_COMMAND,
        PROTOCOL_MAINTENANCE_COMMAND, PROTOCOL_MCP_COMMAND,
    },
    BridgeError,
};

#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

#[derive(Clone, Debug)]
pub struct CliRequest {
    arguments: Vec<OsString>,
    environment: CompanionEnvironment,
}

impl CliRequest {
    pub fn new(arguments: Vec<OsString>) -> Self {
        Self {
            arguments,
            environment: CompanionEnvironment::new(),
        }
    }

    pub fn environment_mut(&mut self) -> &mut CompanionEnvironment {
        &mut self.environment
    }

    pub(crate) fn into_process(self) -> ProcessRequest {
        ProcessRequest {
            command: ProtocolCommand::Cli(self.arguments),
            environment: self.environment,
        }
    }
}

#[derive(Clone, Debug)]
pub struct McpRequest {
    input: Vec<u8>,
    environment: CompanionEnvironment,
}

impl McpRequest {
    pub fn new(input: impl Into<Vec<u8>>) -> Self {
        Self {
            input: input.into(),
            environment: CompanionEnvironment::new(),
        }
    }

    pub fn environment_mut(&mut self) -> &mut CompanionEnvironment {
        &mut self.environment
    }

    pub(crate) fn validate(&self, limits: LimitConfiguration) -> Result<(), BridgeError> {
        if self.input.len() > limits.input_bytes {
            return Err(BridgeError::Limit("input bytes"));
        }
        if self.input.last() != Some(&b'\n')
            || self.input[..self.input.len().saturating_sub(1)].contains(&b'\n')
        {
            return Err(BridgeError::InvalidProtocolResponse("MCP request frame"));
        }
        Ok(())
    }

    pub(crate) fn into_process(self) -> CapturedProcessRequest {
        CapturedProcessRequest {
            control: ProcessRequest {
                command: ProtocolCommand::McpServe,
                environment: self.environment,
            },
            stdin: self.input,
        }
    }
}

#[derive(Clone, Debug)]
pub struct MaintenanceRequest {
    environment: CompanionEnvironment,
}

impl MaintenanceRequest {
    pub fn new() -> Self {
        Self {
            environment: CompanionEnvironment::new(),
        }
    }

    pub fn environment_mut(&mut self) -> &mut CompanionEnvironment {
        &mut self.environment
    }

    pub(crate) fn into_process(self) -> CapturedProcessRequest {
        CapturedProcessRequest {
            control: ProcessRequest {
                command: ProtocolCommand::Maintenance,
                environment: self.environment,
            },
            stdin: Vec::new(),
        }
    }
}

impl Default for MaintenanceRequest {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ProtocolCommand {
    Handshake,
    Cli(Vec<OsString>),
    McpServe,
    Maintenance,
}

impl ProtocolCommand {
    fn arguments(&self) -> Vec<OsString> {
        let mut arguments = vec![OsString::from(PROTOCOL_ENTRYPOINT_ARGUMENT)];
        match self {
            Self::Handshake => arguments.push(OsString::from(PROTOCOL_HANDSHAKE_COMMAND)),
            Self::Cli(cli) => {
                arguments.push(OsString::from(PROTOCOL_CLI_COMMAND));
                arguments.push(OsString::from("--"));
                arguments.extend(cli.iter().cloned());
            }
            Self::McpServe => arguments.push(OsString::from(PROTOCOL_MCP_COMMAND)),
            Self::Maintenance => arguments.push(OsString::from(PROTOCOL_MAINTENANCE_COMMAND)),
        }
        arguments
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ProcessRequest {
    pub(crate) command: ProtocolCommand,
    pub(crate) environment: CompanionEnvironment,
}

impl ProcessRequest {
    pub(crate) fn handshake() -> CapturedProcessRequest {
        CapturedProcessRequest {
            control: Self {
                command: ProtocolCommand::Handshake,
                environment: CompanionEnvironment::new(),
            },
            stdin: Vec::new(),
        }
    }

    pub(crate) fn arguments(&self) -> Vec<OsString> {
        self.command.arguments()
    }

    pub(crate) fn validate(&self, limits: LimitConfiguration) -> Result<(), BridgeError> {
        let arguments = self.arguments();
        if arguments.len() > limits.arguments {
            return Err(BridgeError::Limit("argument count"));
        }
        if self.environment.len() > limits.environment_entries {
            return Err(BridgeError::Limit("environment entry count"));
        }
        let mut control_bytes = 0_usize;
        for argument in &arguments {
            control_bytes = control_bytes
                .checked_add(native_size(argument))
                .and_then(|value| value.checked_add(1))
                .ok_or(BridgeError::Limit("control bytes"))?;
            reject_nul(argument)?;
        }
        for (key, value) in self.environment.iter() {
            control_bytes = control_bytes
                .checked_add(native_size(key))
                .and_then(|total| total.checked_add(native_size(value)))
                .and_then(|total| total.checked_add(2))
                .ok_or(BridgeError::Limit("control bytes"))?;
            validate_environment_name(key)?;
            reject_nul(value)?;
        }
        if control_bytes > limits.control_bytes {
            return Err(BridgeError::Limit("control bytes"));
        }
        Ok(())
    }
}

fn validate_environment_name(value: &OsStr) -> Result<(), BridgeError> {
    let Some(value) = value.to_str() else {
        return Err(BridgeError::InvalidEnvironmentName);
    };
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(BridgeError::InvalidEnvironmentName);
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub(crate) struct CapturedProcessRequest {
    pub(crate) control: ProcessRequest,
    pub(crate) stdin: Vec<u8>,
}

impl CapturedProcessRequest {
    pub(crate) fn validate(&self, limits: LimitConfiguration) -> Result<(), BridgeError> {
        self.control.validate(limits)?;
        if self.stdin.len() > limits.input_bytes {
            return Err(BridgeError::Limit("input bytes"));
        }
        Ok(())
    }
}

#[cfg(unix)]
fn native_size(value: &OsStr) -> usize {
    use std::os::unix::ffi::OsStrExt as _;
    value.as_bytes().len()
}

#[cfg(windows)]
fn native_size(value: &OsStr) -> usize {
    use std::os::windows::ffi::OsStrExt as _;
    value.encode_wide().count().saturating_mul(2)
}

#[cfg(not(any(unix, windows)))]
fn native_size(_value: &OsStr) -> usize {
    usize::MAX
}

#[cfg(unix)]
fn reject_nul(value: &OsStr) -> Result<(), BridgeError> {
    use std::os::unix::ffi::OsStrExt as _;
    if value.as_bytes().contains(&0) {
        Err(BridgeError::Limit("control bytes"))
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn reject_nul(value: &OsStr) -> Result<(), BridgeError> {
    use std::os::windows::ffi::OsStrExt as _;
    if value.encode_wide().any(|unit| unit == 0) {
        Err(BridgeError::Limit("control bytes"))
    } else {
        Ok(())
    }
}

#[cfg(not(any(unix, windows)))]
fn reject_nul(_value: &OsStr) -> Result<(), BridgeError> {
    Err(BridgeError::UnsupportedPlatform)
}
