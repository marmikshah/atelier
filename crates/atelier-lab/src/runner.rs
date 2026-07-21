//! Bounded policy-driven episode loop. The runner owns budgets and feedback;
//! the environment remains the only authority that compiles, executes, and
//! records actions.

use serde::{Deserialize, Serialize};

use crate::action::Stage;
use crate::env::{AtelierEnv, EpisodeResult, PixelArtEnv};
use crate::observation::Observation;
use crate::policy::{
    Policy, PolicyFeedback, PolicyOutcome, PolicyRequest, PolicyUsage, POLICY_FORMAT_VERSION,
};
use crate::task::Task;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerConfig {
    pub max_turns: usize,
    pub max_policy_errors: usize,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        RunnerConfig {
            max_turns: 40,
            max_policy_errors: 3,
        }
    }
}

impl RunnerConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.max_turns == 0 {
            return Err("max_turns must be at least 1".into());
        }
        if self.max_policy_errors == 0 {
            return Err("max_policy_errors must be at least 1".into());
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerTermination {
    Completed,
    TurnLimit,
    PolicyErrorLimit,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunnerReport {
    pub policy: String,
    pub turns: usize,
    pub accepted_actions: usize,
    pub rejected_actions: usize,
    pub policy_errors: usize,
    pub usage: PolicyUsage,
    pub termination: RunnerTermination,
    pub result: EpisodeResult,
}

/// Run one task to an explicit Finished stage or a bounded incomplete finish.
/// Every policy attempt is recorded before its resulting environment step.
pub fn run_policy_episode(
    env: &mut AtelierEnv,
    task: &Task,
    policy: &mut impl Policy,
    config: &RunnerConfig,
) -> Result<RunnerReport, String> {
    config.validate()?;
    if policy.name().trim().is_empty() {
        return Err("policy name cannot be empty".into());
    }
    let mut observation = env.reset(task)?.light().clone();
    let mut previous = None;
    let mut turns = 0;
    let mut accepted_actions = 0;
    let mut rejected_actions = 0;
    let mut policy_errors = 0;
    let mut usage = PolicyUsage::default();

    for turn in 1..=config.max_turns {
        turns = turn;
        let request = PolicyRequest {
            format_version: POLICY_FORMAT_VERSION,
            task: task.clone(),
            observation: observation.clone(),
            turn,
            max_turns: config.max_turns,
            previous: previous.clone(),
        };
        let proposed = policy
            .propose(&request)
            .and_then(|response| response.validate().map(|_| response));
        let response = match proposed {
            Ok(response) => {
                env.record_policy_call(
                    policy.name(),
                    request,
                    PolicyOutcome::Response {
                        response: response.clone(),
                    },
                )?;
                response
            }
            Err(error) => {
                env.record_policy_call(
                    policy.name(),
                    request,
                    PolicyOutcome::Error {
                        error: error.clone(),
                    },
                )?;
                policy_errors += 1;
                previous = Some(PolicyFeedback::PolicyError { error });
                if policy_errors >= config.max_policy_errors {
                    let result = env.finish()?;
                    return Ok(RunnerReport {
                        policy: policy.name().into(),
                        turns,
                        accepted_actions,
                        rejected_actions,
                        policy_errors,
                        usage,
                        termination: RunnerTermination::PolicyErrorLimit,
                        result,
                    });
                }
                continue;
            }
        };

        if let Some(turn_usage) = &response.usage {
            add_usage(&mut usage.input_tokens, turn_usage.input_tokens);
            add_usage(
                &mut usage.cached_input_tokens,
                turn_usage.cached_input_tokens,
            );
            add_usage(&mut usage.output_tokens, turn_usage.output_tokens);
            add_usage(&mut usage.reasoning_tokens, turn_usage.reasoning_tokens);
        }
        let transition = env.step(&response.action)?;
        if transition.accepted {
            accepted_actions += 1;
        } else {
            rejected_actions += 1;
        }
        let failed_tools = transition
            .tool_results
            .iter()
            .filter(|result| !result.ok)
            .map(|result| result.tool.clone())
            .collect();
        previous = Some(PolicyFeedback::Transition {
            accepted: transition.accepted,
            compile_error: transition.error.clone(),
            failed_tools,
        });
        observation = match &transition.observation_after {
            Some(Observation::Light(light)) => light.clone(),
            Some(Observation::Full(full)) => full.light.clone(),
            None => transition.observation_before.light().clone(),
        };

        // `Finish`, or advancing from Cleanup, moves the authoritative env
        // stage to Finished. Close immediately so the final render is stored.
        if transition.accepted && observation.stage == Stage::Finished {
            let result = env.finish()?;
            return Ok(RunnerReport {
                policy: policy.name().into(),
                turns,
                accepted_actions,
                rejected_actions,
                policy_errors,
                usage,
                termination: RunnerTermination::Completed,
                result,
            });
        }
    }

    let result = env.finish()?;
    Ok(RunnerReport {
        policy: policy.name().into(),
        turns,
        accepted_actions,
        rejected_actions,
        policy_errors,
        usage,
        termination: RunnerTermination::TurnLimit,
        result,
    })
}

fn add_usage(total: &mut Option<u64>, value: Option<u64>) {
    if let Some(value) = value {
        *total = Some(total.unwrap_or(0).saturating_add(value));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::PathBuf;

    use super::*;
    use crate::action::{Action, ActionKind, CompileError};
    use crate::policy::{PolicyError, PolicyResponse};
    use crate::task::StyleSpec;

    struct ScriptedPolicy {
        replies: VecDeque<Result<PolicyResponse, PolicyError>>,
        requests: Vec<PolicyRequest>,
    }

    impl Policy for ScriptedPolicy {
        fn name(&self) -> &str {
            "scripted-test"
        }

        fn propose(&mut self, request: &PolicyRequest) -> Result<PolicyResponse, PolicyError> {
            self.requests.push(request.clone());
            self.replies.pop_front().unwrap_or_else(|| {
                Err(PolicyError::Protocol {
                    message: "script exhausted".into(),
                })
            })
        }
    }

    fn task() -> Task {
        Task {
            id: "item-runner-001".into(),
            prompt: "A red potion".into(),
            category: "item".into(),
            width: 32,
            height: 32,
            max_colors: 16,
            must_include: vec!["bottle".into()],
            must_avoid: vec![],
            style: StyleSpec {
                outline: "selective".into(),
                lighting: "upper-left".into(),
                detail: "medium".into(),
            },
            split: "development".into(),
        }
    }

    fn response(
        action: ActionKind,
        input: u64,
        output: u64,
    ) -> Result<PolicyResponse, PolicyError> {
        Ok(PolicyResponse {
            format_version: POLICY_FORMAT_VERSION,
            action: Action::new(action),
            usage: Some(PolicyUsage {
                input_tokens: Some(input),
                cached_input_tokens: None,
                output_tokens: Some(output),
                reasoning_tokens: None,
            }),
        })
    }

    fn root(tag: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("atelier-lab-runner-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn runner_completes_and_feeds_rejections_back() {
        let root = root("complete");
        let mut env = AtelierEnv::new(&root, 4).unwrap();
        let mut policy = ScriptedPolicy {
            replies: VecDeque::from([
                // Illegal in Specification: this must become feedback.
                response(
                    ActionKind::PaintPatch {
                        layer: 0,
                        x: 0,
                        y: 0,
                        width: 1,
                        height: 1,
                        grid: vec![0],
                    },
                    10,
                    2,
                ),
                response(
                    ActionKind::SetPalette {
                        colors: vec![[40, 20, 20, 255], [210, 40, 50, 255]],
                    },
                    11,
                    3,
                ),
                response(ActionKind::AdvanceStage, 12, 1),
                response(
                    ActionKind::PaintPatch {
                        layer: 0,
                        x: 4,
                        y: 4,
                        width: 2,
                        height: 2,
                        grid: vec![1; 4],
                    },
                    13,
                    4,
                ),
                response(ActionKind::Finish, 14, 1),
            ]),
            requests: vec![],
        };
        let report =
            run_policy_episode(&mut env, &task(), &mut policy, &RunnerConfig::default()).unwrap();
        assert_eq!(report.termination, RunnerTermination::Completed);
        assert!(report.result.completed);
        assert_eq!(report.turns, 5);
        assert_eq!(report.accepted_actions, 4);
        assert_eq!(report.rejected_actions, 1);
        assert_eq!(report.usage.input_tokens, Some(60));
        assert_eq!(report.usage.output_tokens, Some(11));
        assert!(matches!(
            policy.requests[1].previous,
            Some(PolicyFeedback::Transition {
                accepted: false,
                compile_error: Some(CompileError::StageViolation { .. }),
                ..
            })
        ));
        let events =
            crate::Recorder::read(&env.episode_dir().join(crate::EPISODE_LOG_FILE)).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.event, crate::EventKind::PolicyCall { .. }))
                .count(),
            5
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn runner_bounds_repeated_policy_failures_and_still_closes_episode() {
        let root = root("errors");
        let mut env = AtelierEnv::new(&root, 5).unwrap();
        let error = PolicyError::Timeout { seconds: 30 };
        let mut policy = ScriptedPolicy {
            replies: VecDeque::from([Err(error.clone()), Err(error)]),
            requests: vec![],
        };
        let report = run_policy_episode(
            &mut env,
            &task(),
            &mut policy,
            &RunnerConfig {
                max_turns: 10,
                max_policy_errors: 2,
            },
        )
        .unwrap();
        assert_eq!(report.termination, RunnerTermination::PolicyErrorLimit);
        assert!(!report.result.completed);
        assert_eq!(report.policy_errors, 2);
        assert_eq!(policy.requests.len(), 2);
        assert!(matches!(
            policy.requests[1].previous,
            Some(PolicyFeedback::PolicyError { .. })
        ));
        let _ = std::fs::remove_dir_all(root);
    }
}
