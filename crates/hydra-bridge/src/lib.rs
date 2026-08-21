#![forbid(unsafe_code)]
//! Generic local process boundary for optional foreign-community adapters.

use std::{
    collections::BTreeMap,
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use uuid::Uuid;

pub const PROTOCOL: &str = "hydra-foreign-community-bridge/v1";
const MAX_MESSAGE_BYTES: usize = 2 * 1024 * 1024;
const MAX_BRIDGE_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("foreign-community bridge input is invalid: {0}")]
    Invalid(String),
    #[error("foreign-community bridge {0} is not installed")]
    Missing(String),
    #[error("foreign-community bridge I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("foreign-community bridge response is invalid: {0}")]
    Protocol(String),
    #[error("foreign-community bridge rejected the request: {0}")]
    Rejected(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BridgeDescriptor {
    pub id: String,
    pub name: String,
    pub version: String,
    pub protocol: String,
    pub capabilities: Vec<String>,
    pub credential_custody: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstalledBridge {
    pub executable: PathBuf,
    pub sha256: String,
    pub descriptor: BridgeDescriptor,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    pub bridges: BTreeMap<String, InstalledBridge>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Request<'a> {
    protocol: &'static str,
    #[serde(rename = "requestId")]
    id: String,
    operation: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    persona_id: Option<&'a str>,
    payload: &'a Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Response {
    protocol: String,
    request_id: String,
    ok: bool,
    result: Option<Value>,
    error: Option<Failure>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Failure {
    message: String,
}

pub struct BridgeRegistry {
    root: PathBuf,
}

impl BridgeRegistry {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Loads the configured adapters. A missing registry is an empty registry.
    ///
    /// # Errors
    /// Returns an error for unreadable or malformed configuration.
    pub fn load(&self) -> Result<Registry, BridgeError> {
        let path = self.registry_path();
        if !path.exists() {
            return Ok(Registry::default());
        }
        let bytes = fs::read(path)?;
        serde_json::from_slice(&bytes).map_err(|error| BridgeError::Protocol(error.to_string()))
    }

    /// Installs a local bridge executable into Hydra's managed adapter directory,
    /// verifies its descriptor, and configures it atomically.
    ///
    /// # Errors
    /// Returns an error for unsafe identifiers, oversized files, protocol mismatch,
    /// failed execution, or persistence failure.
    pub fn install_local(&self, id: &str, source: &Path) -> Result<InstalledBridge, BridgeError> {
        validate_id(id)?;
        let metadata = fs::metadata(source)?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_BRIDGE_BYTES {
            return Err(BridgeError::Invalid(
                "bridge executable size is invalid".to_owned(),
            ));
        }
        let bytes = fs::read(source)?;
        let sha256 = format!("{:x}", Sha256::digest(&bytes));
        let bin = self.root.join("bridges").join("bin");
        fs::create_dir_all(&bin)?;
        let executable = bin.join(executable_name(id));
        let temporary = bin.join(format!(".{id}-{}.tmp", Uuid::new_v4()));
        fs::write(&temporary, bytes)?;
        make_executable(&temporary)?;
        let descriptor = describe_executable(&temporary, &self.bridge_home(id))?;
        if descriptor.id != id
            || descriptor.protocol != PROTOCOL
            || descriptor.credential_custody != "bridge"
        {
            let _ = fs::remove_file(&temporary);
            return Err(BridgeError::Protocol(
                "bridge descriptor does not match the requested adapter".to_owned(),
            ));
        }
        fs::rename(&temporary, &executable)?;
        let installed = InstalledBridge {
            executable,
            sha256,
            descriptor,
        };
        let mut registry = self.load()?;
        registry.bridges.insert(id.to_owned(), installed.clone());
        self.save(&registry)?;
        Ok(installed)
    }

    /// Invokes one installed adapter operation.
    ///
    /// # Errors
    /// Returns an error when the adapter is missing, fails, violates the protocol,
    /// or rejects the request.
    pub fn invoke(
        &self,
        id: &str,
        operation: &str,
        persona: Option<&str>,
        payload: &Value,
    ) -> Result<Value, BridgeError> {
        validate_id(id)?;
        validate_operation(operation)?;
        let installed = self
            .load()?
            .bridges
            .remove(id)
            .ok_or_else(|| BridgeError::Missing(id.to_owned()))?;
        invoke_executable(
            &installed.executable,
            &self.bridge_home(id),
            operation,
            persona,
            payload,
        )
    }

    fn save(&self, registry: &Registry) -> Result<(), BridgeError> {
        let directory = self.root.join("bridges");
        fs::create_dir_all(&directory)?;
        let path = self.registry_path();
        let temporary = directory.join(format!(".registry-{}.tmp", Uuid::new_v4()));
        let bytes = serde_json::to_vec_pretty(registry)
            .map_err(|error| BridgeError::Protocol(error.to_string()))?;
        fs::write(&temporary, bytes)?;
        fs::rename(temporary, path)?;
        Ok(())
    }

    fn registry_path(&self) -> PathBuf {
        self.root.join("bridges").join("registry.json")
    }
    fn bridge_home(&self, id: &str) -> PathBuf {
        self.root.join("bridge-data").join(id)
    }
}

fn describe_executable(executable: &Path, home: &Path) -> Result<BridgeDescriptor, BridgeError> {
    let value = invoke_executable(
        executable,
        home,
        "describe",
        None,
        &Value::Object(serde_json::Map::default()),
    )?;
    serde_json::from_value(value).map_err(|error| BridgeError::Protocol(error.to_string()))
}

fn invoke_executable(
    executable: &Path,
    home: &Path,
    operation: &str,
    persona: Option<&str>,
    payload: &Value,
) -> Result<Value, BridgeError> {
    fs::create_dir_all(home)?;
    let request_id = Uuid::new_v4().to_string();
    let request = serde_json::to_vec(&Request {
        protocol: PROTOCOL,
        id: request_id.clone(),
        operation,
        persona_id: persona,
        payload,
    })
    .map_err(|error| BridgeError::Protocol(error.to_string()))?;
    if request.len() > MAX_MESSAGE_BYTES {
        return Err(BridgeError::Invalid(
            "bridge request is too large".to_owned(),
        ));
    }
    let mut child = Command::new(executable)
        .env_clear()
        .env("FOREIGN_COMMUNITY_BRIDGE_HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or_else(|| BridgeError::Protocol("bridge stdin is unavailable".to_owned()))?
        .write_all(&request)?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(BridgeError::Protocol(format!(
            "bridge exited with {}",
            output.status
        )));
    }
    if output.stdout.len() > MAX_MESSAGE_BYTES {
        return Err(BridgeError::Protocol(
            "bridge response is too large".to_owned(),
        ));
    }
    let response: Response = serde_json::from_slice(&output.stdout)
        .map_err(|error| BridgeError::Protocol(error.to_string()))?;
    if response.protocol != PROTOCOL || response.request_id != request_id {
        return Err(BridgeError::Protocol(
            "bridge response correlation failed".to_owned(),
        ));
    }
    if response.ok {
        response.result.ok_or_else(|| {
            BridgeError::Protocol("successful bridge response has no result".to_owned())
        })
    } else {
        Err(BridgeError::Rejected(response.error.map_or_else(
            || "bridge rejected the request".to_owned(),
            |error| error.message,
        )))
    }
}

fn validate_id(value: &str) -> Result<(), BridgeError> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(BridgeError::Invalid("adapter ID is malformed".to_owned()));
    }
    Ok(())
}

fn validate_operation(value: &str) -> Result<(), BridgeError> {
    if value.is_empty()
        || value.len() > 128
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
        })
    {
        return Err(BridgeError::Invalid(
            "adapter operation is malformed".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn executable_name(id: &str) -> String {
    format!("{id}.exe")
}
#[cfg(not(windows))]
fn executable_name(id: &str) -> String {
    id.to_owned()
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), BridgeError> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions)?;
    Ok(())
}
#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<(), BridgeError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_registry_is_empty() {
        let directory = tempfile::tempdir().unwrap();
        assert!(
            BridgeRegistry::new(directory.path())
                .load()
                .unwrap()
                .bridges
                .is_empty()
        );
    }

    #[test]
    fn adapter_ids_cannot_escape_the_managed_directory() {
        assert!(validate_id("reddit").is_ok());
        assert!(validate_id("../reddit").is_err());
        assert!(validate_id("Reddit").is_err());
    }

    #[test]
    fn operations_are_bounded_tokens() {
        assert!(validate_operation("community.browse").is_ok());
        assert!(validate_operation("oauth; rm").is_err());
    }
}
