//! Provider-neutral policy protocol. A policy sees the task, current light
//! observation, remaining budget, and the previous turn's compact feedback;
//! it returns exactly one validated lab action. Provider SDKs and credentials
//! stay outside the research environment.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::action::{Action, CompileError};
use crate::observation::LightObservation;
use crate::task::Task;

pub const POLICY_FORMAT_VERSION: u32 = 1;
pub const MAX_POLICY_RESPONSE_BYTES: usize = 1_048_576;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PolicyRequest {
    pub format_version: u32,
    pub task: Task,
    pub observation: LightObservation,
    /// One-based model-call number.
    pub turn: usize,
    pub max_turns: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<PolicyFeedback>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PolicyFeedback {
    Transition {
        accepted: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compile_error: Option<CompileError>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        failed_tools: Vec<String>,
    },
    PolicyError {
        error: PolicyError,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PolicyResponse {
    pub format_version: u32,
    pub action: Action,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<PolicyUsage>,
}

impl PolicyResponse {
    pub fn validate(&self) -> Result<(), PolicyError> {
        if self.format_version != POLICY_FORMAT_VERSION {
            return Err(PolicyError::Protocol {
                message: format!(
                    "response format version {} does not match {}",
                    self.format_version, POLICY_FORMAT_VERSION
                ),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PolicyError {
    Launch { message: String },
    Timeout { seconds: u64 },
    Exit { code: Option<i32> },
    ResponseTooLarge { bytes: usize, max: usize },
    InvalidJson { message: String },
    Protocol { message: String },
}

impl std::fmt::Display for PolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PolicyError::Launch { message } => write!(f, "cannot launch policy: {message}"),
            PolicyError::Timeout { seconds } => {
                write!(f, "policy timed out after {seconds}s")
            }
            PolicyError::Exit { code } => write!(f, "policy exited unsuccessfully ({code:?})"),
            PolicyError::ResponseTooLarge { bytes, max } => {
                write!(
                    f,
                    "policy response is {bytes} bytes, over the {max}-byte limit"
                )
            }
            PolicyError::InvalidJson { message } => {
                write!(f, "policy returned invalid JSON: {message}")
            }
            PolicyError::Protocol { message } => write!(f, "policy protocol error: {message}"),
        }
    }
}

impl std::error::Error for PolicyError {}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PolicyOutcome {
    Response { response: PolicyResponse },
    Error { error: PolicyError },
}

pub trait Policy {
    fn name(&self) -> &str;
    fn propose(&mut self, request: &PolicyRequest) -> Result<PolicyResponse, PolicyError>;
}

/// Adapter for any provider wrapper or local model executable. Each call
/// starts the program with no shell, writes one `PolicyRequest` JSON object to
/// stdin, and expects one `PolicyResponse` JSON object on stdout. The child
/// inherits environment variables (where wrappers may read credentials), but
/// neither the environment nor stderr is captured in the episode record.
pub struct CommandPolicy {
    name: String,
    program: PathBuf,
    args: Vec<String>,
    timeout: Duration,
    runtime: tokio::runtime::Runtime,
}

impl CommandPolicy {
    pub fn new(
        name: impl Into<String>,
        program: impl Into<PathBuf>,
        args: Vec<String>,
        timeout: Duration,
    ) -> Result<Self, String> {
        let name = name.into();
        let program = program.into();
        if name.trim().is_empty() {
            return Err("policy name cannot be empty".into());
        }
        if program.as_os_str().is_empty() {
            return Err("policy program cannot be empty".into());
        }
        if timeout.is_zero() {
            return Err("policy timeout must be at least one second".into());
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("cannot build policy runtime: {e}"))?;
        Ok(CommandPolicy {
            name,
            program,
            args,
            timeout,
            runtime,
        })
    }

    fn invoke(&mut self, request: &PolicyRequest) -> Result<Vec<u8>, PolicyError> {
        let payload = serde_json::to_vec(request).map_err(|e| PolicyError::Protocol {
            message: format!("cannot encode request: {e}"),
        })?;
        let timeout = self.timeout;
        let program = self.program.clone();
        let args = self.args.clone();
        self.runtime.block_on(async move {
            use tokio::io::AsyncWriteExt;

            let mut child = tokio::process::Command::new(program)
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                // Stderr may contain provider diagnostics or secrets. Let it
                // reach the operator's terminal but never ingest it as data.
                .stderr(Stdio::inherit())
                .kill_on_drop(true)
                .spawn()
                .map_err(|e| PolicyError::Launch {
                    message: e.to_string(),
                })?;
            let mut stdin = child.stdin.take().ok_or_else(|| PolicyError::Launch {
                message: "child stdin was not piped".into(),
            })?;
            stdin
                .write_all(&payload)
                .await
                .map_err(|e| PolicyError::Launch {
                    message: format!("writing request: {e}"),
                })?;
            stdin
                .write_all(b"\n")
                .await
                .map_err(|e| PolicyError::Launch {
                    message: format!("writing request terminator: {e}"),
                })?;
            drop(stdin);

            let output = tokio::time::timeout(timeout, child.wait_with_output())
                .await
                .map_err(|_| PolicyError::Timeout {
                    seconds: timeout.as_secs(),
                })?
                .map_err(|e| PolicyError::Launch {
                    message: format!("waiting for policy: {e}"),
                })?;
            if !output.status.success() {
                return Err(PolicyError::Exit {
                    code: output.status.code(),
                });
            }
            if output.stdout.len() > MAX_POLICY_RESPONSE_BYTES {
                return Err(PolicyError::ResponseTooLarge {
                    bytes: output.stdout.len(),
                    max: MAX_POLICY_RESPONSE_BYTES,
                });
            }
            Ok(output.stdout)
        })
    }
}

impl Policy for CommandPolicy {
    fn name(&self) -> &str {
        &self.name
    }

    fn propose(&mut self, request: &PolicyRequest) -> Result<PolicyResponse, PolicyError> {
        let bytes = self.invoke(request)?;
        let response: PolicyResponse =
            serde_json::from_slice(&bytes).map_err(|e| PolicyError::InvalidJson {
                message: e.to_string(),
            })?;
        response.validate()?;
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::ActionKind;

    #[test]
    fn policy_response_requires_its_own_version() {
        let response = PolicyResponse {
            format_version: 99,
            action: Action::new(ActionKind::Finish),
            usage: None,
        };
        assert!(matches!(
            response.validate(),
            Err(PolicyError::Protocol { .. })
        ));
    }

    #[test]
    fn policy_error_json_never_has_stderr_or_environment() {
        let error = PolicyError::Exit { code: Some(2) };
        let json = serde_json::to_string(&error).unwrap();
        assert_eq!(json, r#"{"kind":"exit","code":2}"#);
        assert!(!json.contains("stderr"));
        assert!(!json.contains("env"));
    }

    #[cfg(unix)]
    #[test]
    fn command_policy_exchanges_one_json_object() {
        let reply = r#"{"format_version":1,"action":{"action":"Finish"}}"#;
        let script = format!("cat >/dev/null; printf '%s' '{reply}'");
        let mut policy = CommandPolicy::new(
            "shell-fixture",
            "/bin/sh",
            vec!["-c".into(), script],
            Duration::from_secs(2),
        )
        .unwrap();
        let request = PolicyRequest {
            format_version: POLICY_FORMAT_VERSION,
            task: crate::task::Task {
                id: "item-1".into(),
                prompt: "A bottle".into(),
                category: "item".into(),
                width: 32,
                height: 32,
                max_colors: 16,
                must_include: vec![],
                must_avoid: vec![],
                style: crate::task::StyleSpec {
                    outline: "selective".into(),
                    lighting: "upper-left".into(),
                    detail: "medium".into(),
                },
                split: "development".into(),
            },
            observation: crate::observation::LightObservation {
                doc_id: "d".into(),
                width: 32,
                height: 32,
                palette: vec![],
                layers: vec![],
                stage: crate::action::Stage::Specification,
                recent_actions: vec![],
                integrity: crate::observation::IntegrityChecks {
                    on_palette: true,
                    palette_within_budget: true,
                    opaque_pixels: 0,
                },
            },
            turn: 1,
            max_turns: 1,
            previous: None,
        };
        let response = policy.propose(&request).unwrap();
        assert!(matches!(response.action.action, ActionKind::Finish));
    }
}
