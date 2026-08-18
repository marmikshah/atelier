# Roadmap

Atelier development remains on the `1.y.z` release train; feature releases
increment `y`. A new major version is not a roadmap milestone and will not be
created without explicit maintainer authorization. Work is prioritized around
data safety, predictable automation contracts, and bounded resource use. The
editor remains offline and headless; Ubuntu x86_64 and the Alpine amd64
container are the supported runtimes.

## Surface policy

The current 25-tool MCP surface is a budget, not a target to grow. New behavior
should normally be a typed operation on an existing domain tool such as
`doc_frame`, `doc_region`, `doc_palette`, or `doc_export`. A new tool is justified
only when it has a distinct permission, lifecycle, or result contract.

Every addition must preserve these constraints:

- explicit `doc_id`, layer, and frame targets;
- one bounded, atomic document mutation per call;
- deterministic replay for editing operations;
- no process-global selection, clipboard, or active document;
- complete input schemas and structured results;
- a serialized MCP definition set no larger than 32 KiB;
- image and text outputs with explicit size limits.

Free-form batches are intentionally excluded. Bounded ranges or counts are
acceptable when they have one clear operation and fail atomically.

## Current readiness

### Storage and recovery

- Maintain explicit document and journal format versions and reject unknown
  future versions.
- Keep mutation data and its journal entry in one atomic commit.
- Provide read-only store verification with stable machine-readable output.
- Add fault-injection coverage around staging, publication, and recovery.
- Keep the enforced limits for layers, frames, tags, palette entries, cels,
  names, aggregate decoded pixels, recipe size, and recipe length under hostile
  tests. Keep checkpoint count, label, and logical-space quotas covered by the
  same hostile tests.
- Keep the deterministic `.atelierpack` v1 contract covered by round-trip,
  corruption, traversal, collision, and atomic-replacement tests.

### Protocol and security

- Keep CLI, replay, stdio, and HTTP on the same dispatch path.
- Keep registry-derived operation schemas and runtime validation in lockstep.
- Add useful output schemas without exceeding the definition budget.
- Keep network authentication, request limits, host validation, and rooted
  HTTP file access covered by real transport tests.
- Replace pathname revalidation with directory-handle-relative file access for
  rooted HTTP imports/exports, closing the remaining check/use race.
- Separate protocol presentation from editor outcomes so native CLI image
  calls do not perform unnecessary base64 encoding.

### Performance

- Load only the cels required by an operation instead of decoding an entire
  animation for every call.
- Replace full-canvas before/after snapshots with bounded edit-delta tracking.
- Avoid walking and syncing unchanged checkpoint history for ordinary edits.
- Introduce representative large-document benchmarks and enforce regression
  budgets only after their workload is stable.
- Move blocking filesystem and image work away from asynchronous HTTP workers
  while retaining cross-process store ordering; replace the global mutation
  queue with per-document ordering once the store-index lock is separable.
- Segment or compact growing journals and keep immutable checkpoint history out
  of ordinary mutation staging.

### Maintainability

- Split the large MCP server module into dispatch, transaction, result, and
  HTTP-path adapters without changing the public tool registry.
- Split analysis into bounded component, colour, animation, and critique
  modules; keep shared limits and result vocabulary in one place.
- Make the public document constructor fallible, narrow mutation internals,
  and keep persisted-format validation at the core boundary.
- Consolidate no-follow, bounded-file handling around Linux directory handles
  so load, verify, transaction, replay, and HTTP paths cannot drift apart.
- Add fuzz/property tests for operation JSON, malformed metadata/journals, and
  extreme coordinates before widening the stable Rust API.

### Release quality

- Complete the maintainer's line-by-line review of the storage, raster,
  dispatch, HTTP, installer, and release paths.
- Keep the Ubuntu archive and Alpine image smoke-tested from their packaged
  artifacts.
- Add a locked dependency-vulnerability check and publish provenance or an
  SBOM when the release process can verify it end to end.
- Define a compatibility and migration policy before any maintainer-authorized
  major-version change.

## Feature candidates

These candidates improve common workflows without increasing the MCP tool
count:

- `doc_region`: bounded copy and quarter-turn/flip operations within a region;
- `doc_frame`: tag-scoped timing updates and bounded duplication;
- `doc_critique`: optional concise and full profiles over the existing checks;
- `doc_export`: a bundle operation for a sheet, metadata, and selected
  animations in one atomic output directory;
- `doc_palette`: named palette import/export through explicit rooted files;

Candidates are not compatibility commitments. Each needs a concrete use case,
schema and output budgets, replay semantics, and tests before implementation.

## Non-goals

- Restoring implicit editor state or cross-document mutable context.
- Adding a tool for every primitive or report variant.
- Cloud accounts, generation APIs, telemetry, or outbound services.
- Expanding native platform support before the supported Linux releases are
  stable and maintainable.
