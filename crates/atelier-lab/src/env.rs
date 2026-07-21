//! The environment: atelier wrapped as an RL-style task env (lab.md items
//! 6–7). `AtelierEnv` embeds the server crate's `Atelier` in-process — one
//! tokio runtime, `dispatch` for every mutation (so journaling behaves
//! exactly as it does for MCP/CLI callers), direct `Studio` reads for
//! observations (reads rebuild nothing and are never journaled).
//!
//! Isolation is per-episode: each env gets a unique episode directory under
//! a configurable root, its own `Studio` rooted there, and its own seed —
//! the per-tenant model the SaaS shape later inherits. Atelier snapshots the
//! document; the lab pairs it with stage, recent-action history, step count,
//! and terminal state so branching search restores everything the policy sees.
//!
//! Every env records its own episode: an append-only event log
//! (`episode.jsonl`, see `recorder`) and a content-addressed artifact store
//! (`artifacts/`, see `artifacts`) inside the episode directory. Recording
//! is not optional — an episode without its log is training data lost.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use atelier_mcp::server::{is_error_result, result_json, Atelier};
use atelier_studio::{LookOptions, Studio};

use crate::action::{compile, Action, ActionKind, DocSnapshot};
use crate::artifacts::{ArtifactKind, ArtifactRef, ArtifactStore, ARTIFACTS_DIR};
use crate::observation::{
    DocMetadata, FullObservation, IntegrityChecks, LayerObservation, LightObservation, Observation,
    ObservationLevel, Renders,
};
use crate::policy::{PolicyOutcome, PolicyRequest};
use crate::recorder::{
    EventKind, RecordedFullObservation, RecordedObservation, RecordedRenders, Recorder,
};
use crate::storage::Storage;
use crate::task::Task;
use crate::transition::{ToolResult, Transition};

/// Errors are plain strings, matching the `Studio`/`dispatch` APIs this
/// wraps — one error channel end to end.
pub type Result<T> = std::result::Result<T, String>;

/// A checkpoint handle, as minted by `doc_checkpoint save` (`cp<n>`).
pub type CheckpointId = String;

/// Caller identity stamped on every dispatch — shows up in the tool-call
/// logs as the origin of lab-issued calls.
const CALLER: &str = "atelier-lab";

/// How many accepted actions the light observation's recent list retains —
/// enough context for "what did I just do" without growing the state.
const RECENT_ACTIONS_CAP: usize = 8;

/// Upscale factor for the enlarged/notan renders — 8× is what the pairwise
/// UI (lab.md item 18) shows humans.
const ENLARGED_SCALE: u32 = 8;

/// Monotonic per-process episode sequence — with the pid this makes every
/// episode directory unique without a registry.
static EPISODE_SEQ: AtomicU64 = AtomicU64::new(0);

/// The env interface (lab.md item 6).
pub trait PixelArtEnv {
    fn reset(&mut self, task: &Task) -> Result<Observation>;
    fn observe(&mut self, level: ObservationLevel) -> Result<Observation>;
    fn step(&mut self, action: &Action) -> Result<Transition>;
    fn checkpoint(&mut self) -> Result<CheckpointId>;
    fn restore(&mut self, id: &CheckpointId) -> Result<Observation>;
    fn finish(&mut self) -> Result<EpisodeResult>;
}

/// What `finish` returns: the episode's closing state and provenance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EpisodeResult {
    pub episode_id: String,
    pub task_id: String,
    pub seed: u64,
    pub steps: usize,
    pub stage: crate::action::Stage,
    /// True when the episode closed via the Finish action, not a budget cut.
    pub completed: bool,
    pub final_observation: Observation,
}

/// A `PixelArtEnv` backed by an embedded atelier server instance.
pub struct AtelierEnv {
    episode_id: String,
    episode_dir: PathBuf,
    seed: u64,
    /// Current-thread is enough: dispatch serialises on the studio mutex
    /// anyway, and episodes never overlap their own calls.
    runtime: tokio::runtime::Runtime,
    atelier: Atelier,
    studio: Arc<Mutex<Studio>>,
    recorder: Recorder,
    artifacts: ArtifactStore,
    doc_id: Option<String>,
    task: Option<Task>,
    stage: crate::action::Stage,
    recent: std::collections::VecDeque<String>,
    steps: usize,
    finished: bool,
    /// Document checkpoints also capture policy-visible episode state. Search
    /// restores must not leak a rejected branch's stage, history, or budget
    /// usage into the next candidate.
    checkpoints: HashMap<CheckpointId, EnvCheckpoint>,
}

#[derive(Clone)]
struct EnvCheckpoint {
    stage: crate::action::Stage,
    recent: std::collections::VecDeque<String>,
    steps: usize,
    finished: bool,
}

impl AtelierEnv {
    /// A fresh env rooted at `root`: episode dir `<root>/episode-<pid>-<seq>`.
    /// The seed is stored for provenance/replay — atelier itself is
    /// deterministic, so nothing draws from it yet.
    pub fn new(root: impl AsRef<Path>, seed: u64) -> Result<AtelierEnv> {
        let seq = EPISODE_SEQ.fetch_add(1, Ordering::Relaxed);
        let episode_id = format!("episode-{}-{}", std::process::id(), seq);
        let episode_dir = root.as_ref().join(&episode_id);
        let studio = Arc::new(Mutex::new(Studio::with_docs_dir(
            episode_dir.join("documents"),
        )));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("cannot build the tokio runtime: {e}"))?;
        let recorder = Recorder::create(&episode_dir, &episode_id)?;
        let artifacts = ArtifactStore::new(episode_dir.join(ARTIFACTS_DIR))?;
        Ok(AtelierEnv {
            episode_id,
            episode_dir,
            seed,
            runtime,
            atelier: Atelier::with_studio(Arc::clone(&studio)),
            studio,
            recorder,
            artifacts,
            doc_id: None,
            task: None,
            stage: crate::action::Stage::Specification,
            recent: std::collections::VecDeque::new(),
            steps: 0,
            finished: false,
            checkpoints: HashMap::new(),
        })
    }

    pub fn episode_id(&self) -> &str {
        &self.episode_id
    }

    pub fn episode_dir(&self) -> &Path {
        &self.episode_dir
    }

    pub fn seed(&self) -> u64 {
        self.seed
    }

    pub fn stage(&self) -> crate::action::Stage {
        self.stage
    }

    pub fn recorder(&self) -> &Recorder {
        &self.recorder
    }

    pub fn artifacts(&self) -> &ArtifactStore {
        &self.artifacts
    }

    /// Store one binary payload and return its reference — the only way
    /// bytes should reach the episode record (events carry refs, not bytes).
    fn store_artifact(&self, kind: ArtifactKind, bytes: &[u8]) -> Result<ArtifactRef> {
        Ok(ArtifactRef {
            sha256: self.artifacts.put(bytes)?,
            kind,
        })
    }

    /// Append a human/critic judgement to the episode log (lab.md item 11 —
    /// feedback arrives after the steps it judges, so it is its own event).
    pub fn record_feedback(
        &mut self,
        label: impl Into<String>,
        note: Option<String>,
    ) -> Result<()> {
        self.recorder
            .append(EventKind::Feedback {
                label: label.into(),
                note,
            })
            .map(|_| ())
    }

    /// Record one provider-neutral policy exchange. Credentials and provider
    /// stderr never enter these typed values; the request/response and usage
    /// needed to reproduce or price the trajectory do.
    pub fn record_policy_call(
        &mut self,
        policy: &str,
        request: PolicyRequest,
        outcome: PolicyOutcome,
    ) -> Result<()> {
        if policy.trim().is_empty() {
            return Err("policy name cannot be empty".into());
        }
        self.recorder
            .append(EventKind::PolicyCall {
                policy: policy.into(),
                request,
                outcome,
            })
            .map(|_| ())
    }

    /// Poison recovery like the server's: a panicked tool call must not
    /// brick the episode — documents load/save per op, so the guarded state
    /// is not meaningfully corrupted.
    fn studio(&self) -> MutexGuard<'_, Studio> {
        self.studio
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn doc_id(&self) -> Result<&str> {
        self.doc_id
            .as_deref()
            .ok_or_else(|| "no document — reset the episode first".to_string())
    }

    /// Dispatch one tool call, capturing the outcome instead of short-
    /// circuiting: a tool error is transition data (accepted: false), not an
    /// env failure.
    fn dispatch_capture(&self, tool: &str, args: Value) -> ToolResult {
        match self
            .runtime
            .block_on(self.atelier.dispatch(tool, args, CALLER))
        {
            Ok(r) => ToolResult {
                tool: tool.into(),
                ok: !is_error_result(&r),
                result: result_json(&r).unwrap_or(Value::Null),
            },
            Err(e) => ToolResult {
                tool: tool.into(),
                ok: false,
                result: json!({"error": e.to_string()}),
            },
        }
    }

    /// Dispatch that treats a tool error as an env error — for the env's own
    /// bookkeeping calls (create/delete/checkpoint), where a failure means
    /// the episode is broken, not that the policy proposed badly.
    fn dispatch_ok(&self, tool: &str, args: Value) -> Result<Value> {
        let r = self.dispatch_capture(tool, args);
        if r.ok {
            Ok(r.result)
        } else {
            Err(format!("{tool} failed: {}", r.result))
        }
    }

    fn observe_light(&self) -> Result<LightObservation> {
        let doc_id = self.doc_id()?;
        let studio = self.studio();
        let info = studio.doc_info(doc_id)?;
        let palette: Vec<[u8; 4]> = serde_json::from_value(info["palette"].clone())
            .map_err(|e| format!("doc_info palette: {e}"))?;
        let layers_meta = info["layers"]
            .as_array()
            .ok_or("doc_info has no layers array")?;
        let mut layers = Vec::with_capacity(layers_meta.len());
        let mut on_palette = true;
        let mut opaque_pixels = 0usize;
        for (index, l) in layers_meta.iter().enumerate() {
            // A cel that won't index (off-palette pixels) degrades to an
            // empty raster plus the integrity flag — observe must still
            // answer, or a policy could never see the state it caused.
            let indices = match studio.doc_indexed_raster(doc_id, index, 0) {
                Ok(v) => serde_json::from_value::<Vec<Option<u32>>>(v["indices"].clone())
                    .map_err(|e| format!("indexed_raster indices: {e}"))?,
                Err(_) => {
                    on_palette = false;
                    Vec::new()
                }
            };
            opaque_pixels += indices.iter().flatten().count();
            layers.push(LayerObservation {
                index,
                name: l["name"].as_str().unwrap_or("?").into(),
                visible: l["visible"].as_bool().unwrap_or(true),
                opacity: l["opacity"].as_u64().unwrap_or(255) as u8,
                indices,
            });
        }
        let palette_within_budget = match &self.task {
            Some(t) => palette.len() <= t.max_colors as usize,
            None => true,
        };
        Ok(LightObservation {
            doc_id: doc_id.into(),
            width: info["w"].as_u64().unwrap_or(0) as u32,
            height: info["h"].as_u64().unwrap_or(0) as u32,
            palette,
            layers,
            stage: self.stage,
            recent_actions: self.recent.iter().cloned().collect(),
            integrity: IntegrityChecks {
                on_palette,
                palette_within_budget,
                opaque_pixels,
            },
        })
    }

    fn observe_full(&self) -> Result<FullObservation> {
        let light = self.observe_light()?;
        let doc_id = light.doc_id.clone();
        let studio = self.studio();
        let look = |mode: &str, scale: u32| -> Result<Vec<u8>> {
            let (png, _) = studio.look(
                &doc_id,
                0,
                &LookOptions {
                    mode: mode.into(),
                    scale: Some(scale),
                    ..LookOptions::default()
                },
            )?;
            Ok(png)
        };
        let renders = Renders {
            native: studio.render_png_bytes(&doc_id, 0, 1)?,
            enlarged: studio.render_png_bytes(&doc_id, 0, ENLARGED_SCALE)?,
            grayscale: look("value", 1)?,
            notan: look("notan", ENLARGED_SCALE)?,
        };
        let info = studio.doc_info(&doc_id)?;
        let doc = DocMetadata {
            id: doc_id.clone(),
            name: info["name"].as_str().unwrap_or(&doc_id).into(),
            width: light.width,
            height: light.height,
            layer_count: info["layers"].as_array().map(Vec::len).unwrap_or(0),
            frame_count: info["frames"].as_array().map(Vec::len).unwrap_or(0),
            palette_len: light.palette.len(),
        };
        Ok(FullObservation {
            light,
            renders,
            palette_report: studio.doc_palette_report(&doc_id, Some(0), None, None, 8)?,
            components: studio.doc_components(&doc_id, 0, None, 8, None, 1)?,
            critique: studio.critique(&doc_id, 0, None, None)?,
            doc,
        })
    }
}

impl PixelArtEnv for AtelierEnv {
    /// Fresh episode state: a new document at the task's canvas size (cels
    /// start empty — the transparent background the scope requires), stage
    /// back at Specification, recent actions and step count cleared. A doc
    /// left over from a previous reset on this env is deleted first.
    fn reset(&mut self, task: &Task) -> Result<Observation> {
        task.validate()?;
        if let Some(old) = self.doc_id.take() {
            self.dispatch_ok("delete_doc", json!({"doc_id": old}))?;
        }
        let created = self.dispatch_ok(
            "doc_create",
            json!({"name": task.id, "width": task.width, "height": task.height}),
        )?;
        self.doc_id = Some(
            created["id"]
                .as_str()
                .ok_or("doc_create returned no id")?
                .to_string(),
        );
        self.task = Some(task.clone());
        self.stage = crate::action::Stage::Specification;
        self.recent.clear();
        self.steps = 0;
        self.finished = false;
        self.checkpoints.clear();
        tracing::info!(episode = %self.episode_id, task = %task.id, "episode reset");
        let observation = self.observe_light()?;
        self.recorder.append(EventKind::Reset {
            task: task.clone(),
            observation: observation.clone(),
        })?;
        Ok(Observation::Light(observation))
    }

    /// Observe the current state, recording the read — the atelier journal
    /// omits reads, and an episode log without them can't show what the
    /// policy was looking at when it chose an action. Full observations put
    /// their render bytes in the artifact store and log the references.
    fn observe(&mut self, level: ObservationLevel) -> Result<Observation> {
        let (obs, recorded) = match level {
            ObservationLevel::Light => {
                let l = self.observe_light()?;
                (Observation::Light(l.clone()), RecordedObservation::Light(l))
            }
            ObservationLevel::Full => {
                let f = self.observe_full()?;
                let recorded = RecordedObservation::Full(Box::new(RecordedFullObservation {
                    light: f.light.clone(),
                    renders: RecordedRenders {
                        native: self
                            .store_artifact(ArtifactKind::RenderNative, &f.renders.native)?,
                        enlarged: self
                            .store_artifact(ArtifactKind::RenderEnlarged, &f.renders.enlarged)?,
                        grayscale: self
                            .store_artifact(ArtifactKind::RenderGrayscale, &f.renders.grayscale)?,
                        notan: self.store_artifact(ArtifactKind::RenderNotan, &f.renders.notan)?,
                    },
                    palette_report: f.palette_report.clone(),
                    components: f.components.clone(),
                    critique: f.critique.clone(),
                    doc: f.doc.clone(),
                }));
                (Observation::Full(Box::new(f)), recorded)
            }
        };
        self.recorder.append(EventKind::Observation {
            observation: recorded,
        })?;
        Ok(obs)
    }

    /// One action: compile against the current light observation (rejection
    /// costs no tool calls and touches nothing), dispatch the compiled calls
    /// in order, then observe again. Compile rejections and mid-sequence
    /// tool errors are `accepted: false` transitions, not env errors — and
    /// both paths land in the episode log: a rejected action the record
    /// silently dropped would fake a better invalid-action rate.
    fn step(&mut self, action: &Action) -> Result<Transition> {
        if self.finished {
            return Err("episode is finished — reset before stepping again".into());
        }
        let before = self.observe_light()?;
        let snapshot = DocSnapshot {
            doc_id: before.doc_id.clone(),
            width: before.width,
            height: before.height,
            palette: before.palette.clone(),
            max_colors: self
                .task
                .as_ref()
                .map(|task| task.max_colors as usize)
                .ok_or("no task — reset the episode first")?,
            stage: self.stage,
            layers: before.layers.iter().map(|l| l.indices.clone()).collect(),
        };
        let transition = match compile(action, &snapshot) {
            Err(error) => {
                tracing::debug!(episode = %self.episode_id, %error, "action rejected");
                Transition {
                    observation_before: Observation::Light(before.clone()),
                    action: action.clone(),
                    compiled: Vec::new(),
                    tool_results: Vec::new(),
                    observation_after: None,
                    accepted: false,
                    error: Some(error),
                }
            }
            Ok(compiled) => {
                // A DSL action is one transition even when it compiles to
                // several Atelier calls. Snapshot multi-call actions so a
                // failure cannot leave behind a half-applied rejected edit.
                let transaction = if compiled.len() > 1 {
                    let saved = self.dispatch_ok(
                        "doc_checkpoint",
                        json!({"doc_id": self.doc_id()?, "action": "save", "label": "lab-transaction"}),
                    )?;
                    Some(
                        saved["saved"]
                            .as_str()
                            .ok_or_else(|| {
                                format!("transaction checkpoint returned no id: {saved}")
                            })?
                            .to_string(),
                    )
                } else {
                    None
                };
                let mut tool_results = Vec::with_capacity(compiled.len());
                let mut accepted = true;
                for c in &compiled {
                    let r = self.dispatch_capture(&c.tool, c.args.clone());
                    accepted &= r.ok;
                    tool_results.push(r);
                    if !accepted {
                        break;
                    }
                }
                if let Some(checkpoint_id) = transaction {
                    if !accepted {
                        self.dispatch_ok(
                            "doc_checkpoint",
                            json!({
                                "doc_id": self.doc_id()?,
                                "action": "restore",
                                "checkpoint_id": checkpoint_id,
                            }),
                        )?;
                    }
                    self.dispatch_ok(
                        "doc_checkpoint",
                        json!({
                            "doc_id": self.doc_id()?,
                            "action": "prune",
                            "checkpoint_id": checkpoint_id,
                        }),
                    )?;
                }
                if accepted {
                    match &action.action {
                        ActionKind::AdvanceStage => {
                            self.stage = self.stage.next().unwrap_or(self.stage);
                        }
                        ActionKind::Finish => self.stage = crate::action::Stage::Finished,
                        _ => {}
                    }
                    self.recent.push_back(action.summarize());
                    if self.recent.len() > RECENT_ACTIONS_CAP {
                        self.recent.pop_front();
                    }
                    self.steps += 1;
                }
                let after = self.observe_light()?;
                Transition {
                    observation_before: Observation::Light(before.clone()),
                    action: action.clone(),
                    compiled,
                    tool_results,
                    observation_after: Some(Observation::Light(after)),
                    accepted,
                    error: None,
                }
            }
        };
        self.recorder.append(EventKind::Step {
            observation_before: before,
            // Keep the dedicated field for the frozen event contract while
            // sourcing it from the action metadata the model actually emits.
            intent: transition.action.intent.clone(),
            action: transition.action.clone(),
            compiled: transition.compiled.clone(),
            tool_results: transition.tool_results.clone(),
            observation_after: transition
                .observation_after
                .as_ref()
                .map(|o| o.light().clone()),
            accepted: transition.accepted,
            error: transition.error.clone(),
        })?;
        Ok(transition)
    }

    fn checkpoint(&mut self) -> Result<CheckpointId> {
        let v = self.dispatch_ok(
            "doc_checkpoint",
            json!({"doc_id": self.doc_id()?, "action": "save"}),
        )?;
        let checkpoint_id = v["saved"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| format!("doc_checkpoint save returned no id: {v}"))?;
        self.recorder.append(EventKind::Checkpoint {
            checkpoint_id: checkpoint_id.clone(),
        })?;
        self.checkpoints.insert(
            checkpoint_id.clone(),
            EnvCheckpoint {
                stage: self.stage,
                recent: self.recent.clone(),
                steps: self.steps,
                finished: self.finished,
            },
        );
        Ok(checkpoint_id)
    }

    /// Restore a document checkpoint and its matching policy-visible episode
    /// state, then answer the result.
    fn restore(&mut self, id: &CheckpointId) -> Result<Observation> {
        let state = self
            .checkpoints
            .get(id)
            .cloned()
            .ok_or_else(|| format!("checkpoint '{id}' has no environment state"))?;
        self.dispatch_ok(
            "doc_checkpoint",
            json!({"doc_id": self.doc_id()?, "action": "restore", "checkpoint_id": id}),
        )?;
        self.stage = state.stage;
        self.recent = state.recent;
        self.steps = state.steps;
        self.finished = state.finished;
        let observation = self.observe_light()?;
        self.recorder.append(EventKind::Restore {
            checkpoint_id: id.clone(),
            observation: observation.clone(),
        })?;
        Ok(Observation::Light(observation))
    }

    fn finish(&mut self) -> Result<EpisodeResult> {
        if self.finished {
            return Err("episode is already finished".into());
        }
        let final_observation = Observation::Light(self.observe_light()?);
        let result = EpisodeResult {
            episode_id: self.episode_id.clone(),
            task_id: self
                .task
                .as_ref()
                .map(|t| t.id.clone())
                .ok_or("no task — reset the episode first")?,
            seed: self.seed,
            steps: self.steps,
            stage: self.stage,
            completed: self.stage == crate::action::Stage::Finished,
            final_observation,
        };
        // The closing render is the replay gate's reference image (lab.md
        // item 12), so it is stored content-addressed with the episode, not
        // recomputed later from a store that may be gone.
        let png = self.studio().render_png_bytes(self.doc_id()?, 0, 1)?;
        let final_render = self.store_artifact(ArtifactKind::FinalImage, &png)?;
        self.recorder.append(EventKind::Finish {
            result: result.clone(),
            final_render,
        })?;
        self.finished = true;
        Ok(result)
    }
}
