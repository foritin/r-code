//! P21 — separate dependency preparation with immutable cache promotion.
//!
//! Claims are pinned through the public API only, on real material: a real
//! candidate workspace, a real `materialize`d private tree, and real child
//! processes started by a bound [`DependencyPrepRoute`]. Preparation no longer
//! rides the shell-shaped `CommandExecutionBackend`, so a route is handed the
//! whole frozen spec and drops a sentinel in the run's private tree only when
//! it actually runs: every zero-spawn claim below carries a control arm proving
//! that sentinel is writable. Acceptance ①/②/③, INV-06 and INV-07 are asserted
//! where P21 puts them at risk.

use r_code_core::error::ProductError;
use r_code_gateway::execution_backend::{
    CollectedOutput, CommandExecutionBackend, CommandHandle, CommandSpec, LocalShellBackend,
};
use r_code_harness_protocol::canonical_input_hash;
use r_code_harness_protocol::services::{NetworkCeiling, PermissionCeiling, WorkUnitEffectClass};
use r_code_kernel::verification::{CheckDefinition, CheckEntrypoint};
use r_code_runtime::services::artifacts::sha256_hex;
use r_code_runtime::services::authorization::{
    apply_approval_decision, AuthorizationDecision, AuthorizationService, CredentialScope,
    DenyReason, EffectivePermissions, OperationDescriptor, PrepNetworkAuthority,
    WorkspaceCapability,
};
use r_code_runtime::services::execution::{
    CheckSandboxGate, CheckSpawnSpec, CheckSpecError, DependencyPrepKind, DependencyPrepRoute,
    DependencyPrepSpec, ExecutionError, ExecutionService, PrepCachePaths, PrepGate,
    SandboxedCheckBackend, SandboxedPrepBackend,
};
use r_code_runtime::services::launch_profiles::{install_profile_capability, ProfileSource};
use r_code_runtime::services::verification::{CheckOutcome, CheckStatus, VerificationRunner};
use r_code_runtime::services::verification_inputs::*;
use r_code_runtime::services::workspaces::{CandidateManifest, TaskWorkspaceBinding};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

type SpecEdit = (&'static str, fn(&mut DependencyPrepSpec));

/// The toolchain identity frozen with every tree prepared below.
const TOOLCHAIN: &str = "cargo-test";
/// The refusal a closed gate carries, asserted verbatim on both routes.
const CLOSED: &str = "sandbox-disabled: prep gate closed";
/// A host credential variable a preparation must never hand to a process.
const PROVIDER_TOKEN: &str = "R21QA_PROVIDER_TOKEN";
/// The published-slot validation record the cache implementation writes.
const VALID_MARKER: &str = ".preparation-valid";
/// The poison record `poison` writes inside the staging area.
const POISON_MARKER: &str = ".preparation-poisoned";
/// The exact frozen argument pairs P21.2 requires of each toolchain.
const CARGO_ARGS: &[&str] = &["fetch", "--locked"];
const NPM_ARGS: &[&str] = &["ci", "--ignore-scripts"];
/// The modes the stand-in toolchain is written for.
const TOOL_MODES: [&str; 5] = ["bytes", "empty", "fail", "pkg", "git"];
/// One process-global counter so every bound route owns a distinct sentinel.
static ROUTE_ID: AtomicU64 = AtomicU64::new(1);

/// A bound route's footprint: a file in the run's private tree that only the
/// route itself can create, so an absent one proves zero spawn.
fn sentinel_name(id: u64) -> String {
    format!("route-{id}.log")
}

fn owned(values: &[&str]) -> Vec<String> {
    values.iter().copied().map(String::from).collect()
}

/// The forward-slash canonical form the runtime digests every path into.
fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Whether a path component would make the git directory visible to a sandboxed
/// process, re-derived here so INV-06 is not only ever proven by the runtime's
/// own helper.
fn visible_git_root(root: &str) -> bool {
    root.split(['/', '\\'])
        .any(|component| component.eq_ignore_ascii_case(".git"))
}

/// The environment names a real toolchain needs on this host, none of which
/// addresses a credential. A native route inherits these by name and nothing
/// else, which is what the allowlist arm of acceptance ② relies on.
fn host_base_keys() -> Vec<&'static str> {
    let mut keys = vec!["PATH"];
    if cfg!(windows) {
        keys.extend(["SystemRoot", "COMSPEC", "TEMP", "TMP"]);
    }
    keys
}

/// The three cache layers over a throwaway tree, for spec-only refusals.
fn layers(temp: &Path) -> PrepCachePaths {
    PrepCachePaths {
        lower_cache_readonly: temp.join("cache/lower/id"),
        overlay: temp.join("tree/.prep-overlay"),
        promoted_cache: temp.join("cache/promoted/id"),
    }
}

/// Freeze prep material over `<temp>/tree`, reporting the refusal.
fn spec_at(
    temp: &Path,
    kind: DependencyPrepKind,
    arguments: &[&str],
    network: NetworkCeiling,
    environment: &[&str],
) -> Result<DependencyPrepSpec, CheckSpecError> {
    DependencyPrepSpec::build(
        kind,
        &owned(arguments),
        &temp.join("tree"),
        TOOLCHAIN,
        network,
        &layers(temp),
        &owned(environment),
    )
}

/// Spawnable prep material, varying only what a case names.
fn prep_material(
    temp: &Path,
    kind: DependencyPrepKind,
    arguments: &[&str],
    environment: &[&str],
) -> DependencyPrepSpec {
    spec_at(temp, kind, arguments, NetworkCeiling::Offline, environment).expect("prep material")
}

/// A JS tool standing in for the sandboxed toolchain: it runs as a real child
/// process and deposits its result in the run overlay, where a validated fetch
/// would land. `empty` produces nothing; `fail` produces bytes then exits 3;
/// `pkg` and `git` produce bytes that must never leave the private tree.
fn tool_source(overlay: &Path, mode: &str) -> String {
    let staged = match mode {
        "pkg" => "registry/node_modules/dep/index.js",
        "git" => "registry/.git/config",
        _ => "registry/dep.bin",
    };
    format!(
        "const fs = require('fs');\n\
         const path = require('path');\n\
         const overlay = {:?};\n\
         const staged = path.join(overlay, {:?});\n\
         if ({:?} !== 'empty') {{\n\
         \x20 fs.mkdirSync(path.dirname(staged), {{ recursive: true }});\n\
         \x20 fs.writeFileSync(staged, 'validated-bytes');\n\
         }}\n\
         console.log('token=' + (process.env.{} || 'absent'));\n\
         if ({:?} === 'fail') {{ process.exit(3); }}\n",
        slash(overlay),
        staged,
        mode,
        PROVIDER_TOKEN,
        mode
    )
}

/// A real candidate workspace plus its materialized private tree and cache.
struct Prep {
    temp: tempfile::TempDir,
    dir: PathBuf,
    binding: TaskWorkspaceBinding,
    manifest: CandidateManifest,
    definition: CheckDefinition,
    prepared: PreparedVerificationDir,
    cache: DependencyCache,
}

impl Prep {
    fn with_toolchain(label: &str, lock: &str, toolchain: &str) -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let project = temp.path().join("project");
        let dir = temp.path().join(format!("verify-{label}"));
        std::fs::create_dir_all(project.join("src")).expect("project dirs");
        std::fs::write(project.join("Cargo.toml"), "[package]\nname = \"q21\"\n").expect("toml");
        std::fs::write(project.join("Cargo.lock"), lock).expect("lock");
        std::fs::write(project.join("src/lib.rs"), "pub fn ok() {}\n").expect("source");
        std::fs::write(project.join("verify.js"), "process.exit(0);\n").expect("verifier");
        for mode in TOOL_MODES {
            std::fs::write(
                project.join(format!("prep-{mode}.js")),
                tool_source(&overlay_path(&dir), mode),
            )
            .expect("prep tool");
        }
        let binding = TaskWorkspaceBinding::bind_local("task-s21", &project, &[]).expect("bind");
        let manifest = CandidateManifest::capture(&binding).expect("capture");
        let definition = CheckDefinition {
            check_id: "check:s21".into(),
            entrypoint: CheckEntrypoint::Command {
                program: "node".into(),
                argv: owned(&["verify.js"]),
            },
            control_files: vec![],
            source_roots: owned(&["src"]),
            dependency_locks: owned(&["Cargo.lock"]),
            toolchain: toolchain.into(),
            declared_external_inputs: vec![],
            entrypoint_bytes: None,
        };
        let controls = FrozenControlStore::new(temp.path().join("frozen"));
        let prepared =
            materialize(&binding, &manifest, &controls, &definition, &dir).expect("materialize");
        let cache = DependencyCache::new(&default_cache_root(&dir)).expect("absolute cache root");
        Self {
            temp,
            dir,
            binding,
            manifest,
            definition,
            prepared,
            cache,
        }
    }

    fn new(label: &str) -> Self {
        Self::with_toolchain(label, "version = 3\n", TOOLCHAIN)
    }

    fn controls(&self) -> FrozenControlStore {
        FrozenControlStore::new(self.temp.path().join("frozen"))
    }

    fn identity(&self) -> &str {
        &self.prepared.cache_identity
    }

    fn overlay(&self) -> &Path {
        &self.prepared.overlay
    }

    fn slot(&self) -> PathBuf {
        self.cache
            .promoted(self.identity())
            .expect("digest identity")
    }

    fn state(&self) -> CacheSlotState {
        self.cache.state(self.identity())
    }

    /// The cache layers a preparation of this tree mounts.
    fn layers(&self) -> PrepCachePaths {
        PrepCachePaths {
            lower_cache_readonly: self.cache.lower(self.identity()).expect("lower layer"),
            overlay: self.prepared.overlay.clone(),
            promoted_cache: self.slot(),
        }
    }

    fn spec(&self, network: NetworkCeiling, environment: &[&str]) -> DependencyPrepSpec {
        DependencyPrepSpec::build(
            DependencyPrepKind::Cargo,
            &owned(CARGO_ARGS),
            &self.dir,
            TOOLCHAIN,
            network,
            &self.layers(),
            &owned(environment),
        )
        .expect("spawnable prep material")
    }

    fn descriptor(&self, authority: PrepNetworkAuthority) -> OperationDescriptor {
        OperationDescriptor::dependency_preparation(
            "cargo",
            owned(CARGO_ARGS),
            Some(self.dir.to_string_lossy().to_string()),
            authority,
        )
    }

    fn workspace(&self) -> WorkspaceCapability {
        WorkspaceCapability::WriteWithin {
            root: slash(&self.dir),
        }
    }

    /// An offline preparation with exactly `environment` inherited.
    fn offline(&self, kind: DependencyKind, environment: &[&str]) -> PreparationRequest {
        PreparationRequest::offline(
            PreparationRequest::required_arguments(kind),
            owned(environment),
            Duration::from_secs(60),
        )
    }

    /// A networked preparation carrying whatever authority the caller claims.
    fn fetching(&self, authority: PrepNetworkAuthority) -> PreparationRequest {
        PreparationRequest {
            arguments: PreparationRequest::required_arguments(DependencyKind::Cargo),
            network: NetworkCeiling::PublicInternetClient,
            authority,
            environment_keys: vec![],
            timeout: Duration::from_secs(60),
        }
    }

    /// Run one preparation of this tree over a harness that owns the gate, the
    /// route behind it and a shell that must never be reached.
    async fn prepare(
        &self,
        harness: &Harness,
        kind: DependencyKind,
        request: &PreparationRequest,
    ) -> Result<PreparationOutcome, MaterializeError> {
        let service = prep_service(harness, kind.prep_kind()).await;
        prepare_dependencies(&service, &self.prepared, kind, &self.cache, request).await
    }

    async fn prepare_cargo(
        &self,
        harness: &Harness,
        environment: &[&str],
    ) -> Result<PreparationOutcome, MaterializeError> {
        let request = self.offline(DependencyKind::Cargo, environment);
        self.prepare(harness, DependencyKind::Cargo, &request).await
    }
}

/// The stand-in native sandbox route: it records the whole frozen spec it was
/// handed, then runs a real child over the material that spec names, and drops
/// a sentinel only in the tree it actually executed in. Reaching it at all is
/// only possible through an Activated prep gate with this route bound.
struct FakePrep {
    id: u64,
    script: &'static str,
    honours_allowlist: bool,
    specs: Mutex<Vec<DependencyPrepSpec>>,
    aborts: Mutex<Vec<bool>>,
    timeouts: Mutex<Vec<u64>>,
    stdout: Mutex<Vec<String>>,
}

impl FakePrep {
    fn new(script: &'static str, honours_allowlist: bool) -> Self {
        Self {
            id: ROUTE_ID.fetch_add(1, Ordering::SeqCst),
            script,
            honours_allowlist,
            specs: Mutex::new(Vec::new()),
            aborts: Mutex::new(Vec::new()),
            timeouts: Mutex::new(Vec::new()),
            stdout: Mutex::new(Vec::new()),
        }
    }

    fn specs(&self) -> Vec<DependencyPrepSpec> {
        self.specs.lock().expect("specs").clone()
    }

    fn spawns(&self) -> usize {
        self.specs.lock().expect("specs").len()
    }

    fn aborts(&self) -> Vec<bool> {
        self.aborts.lock().expect("aborts").clone()
    }

    fn timeouts(&self) -> Vec<u64> {
        self.timeouts.lock().expect("timeouts").clone()
    }

    fn stdout(&self) -> String {
        self.stdout.lock().expect("stdout").join("\n")
    }

    fn sentinel(&self, cwd: &Path) -> PathBuf {
        cwd.join(sentinel_name(self.id))
    }
}

#[async_trait::async_trait]
impl DependencyPrepRoute for FakePrep {
    fn route_id(&self) -> &'static str {
        "fake-native-prep-route"
    }

    async fn run(
        &self,
        spec: &DependencyPrepSpec,
        abort_flag: Option<&AtomicBool>,
        timeout: Duration,
    ) -> Result<CollectedOutput, ProductError> {
        self.specs.lock().expect("specs").push(spec.clone());
        self.aborts
            .lock()
            .expect("aborts")
            .push(abort_flag.is_some());
        self.timeouts
            .lock()
            .expect("timeouts")
            .push(timeout.as_millis() as u64);
        std::fs::write(self.sentinel(&spec.cwd), spec.command_line())
            .map_err(|error| ProductError::Other(format!("route sentinel: {error}")))?;
        let mut command = tokio::process::Command::new("node");
        command.arg(self.script).current_dir(&spec.cwd);
        if self.honours_allowlist {
            command.env_clear();
            for key in &spec.environment_keys {
                if let Ok(value) = std::env::var(key) {
                    command.env(key, value);
                }
            }
        }
        let output = tokio::time::timeout(timeout, command.output())
            .await
            .map_err(|_| ProductError::Other("preparation exceeded its deadline".to_string()))?
            .map_err(|error| ProductError::Other(error.to_string()))?;
        self.stdout
            .lock()
            .expect("stdout")
            .push(String::from_utf8_lossy(&output.stdout).to_string());
        Ok(CollectedOutput {
            exit_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }
}

/// A shell-shaped backend that would run anything handed to it. Preparation
/// must never reach it, so an empty record is the INV-07 proof.
#[derive(Default)]
struct ShellTripwire {
    fired: Mutex<Vec<String>>,
}

impl ShellTripwire {
    fn fired(&self) -> Vec<String> {
        self.fired.lock().expect("fired").clone()
    }
}

#[async_trait::async_trait]
impl CommandExecutionBackend for ShellTripwire {
    fn backend_id(&self) -> &'static str {
        "tripwire-shell"
    }

    async fn spawn(
        &self,
        spec: &CommandSpec,
        _abort: Option<&AtomicBool>,
    ) -> Result<CommandHandle, ProductError> {
        self.fired.lock().expect("fired").push(spec.command.clone());
        Err(ProductError::Other(
            "preparation reached a shell route".to_string(),
        ))
    }

    async fn collect(
        &self,
        _handle: CommandHandle,
        _spec: &CommandSpec,
        _abort: Option<&AtomicBool>,
    ) -> Result<CollectedOutput, ProductError> {
        Err(ProductError::Other(
            "preparation reached a shell route".to_string(),
        ))
    }
}

/// The three pieces one preparation call resolves over: the gate, the route
/// behind it (which may be absent) and a shell that must stay untouched.
struct Harness {
    gate: Arc<SandboxedPrepBackend>,
    route: Option<Arc<FakePrep>>,
    shell: Arc<ShellTripwire>,
}

impl Harness {
    fn build(gate: PrepGate, route: Option<FakePrep>) -> Self {
        let route = route.map(Arc::new);
        let bound: Option<Arc<dyn DependencyPrepRoute>> = match route.as_ref() {
            Some(route) => Some(Arc::clone(route) as Arc<dyn DependencyPrepRoute>),
            None => None,
        };
        Self {
            gate: Arc::new(SandboxedPrepBackend::new(gate, bound)),
            route,
            shell: Arc::new(ShellTripwire::default()),
        }
    }

    fn activated(script: &'static str) -> Self {
        Self::build(PrepGate::Activated, Some(FakePrep::new(script, false)))
    }

    /// An Activated gate over a route that builds the child environment from
    /// exactly `environment_keys`, the way a native sandbox must.
    fn filtered(script: &'static str) -> Self {
        Self::build(PrepGate::Activated, Some(FakePrep::new(script, true)))
    }

    fn closed(script: &'static str) -> Self {
        Self::build(
            PrepGate::Closed {
                reason: CLOSED.to_string(),
            },
            Some(FakePrep::new(script, false)),
        )
    }

    /// The honest v1 shape: an Activated verdict with nothing bound behind it.
    fn unbound() -> Self {
        Self::build(PrepGate::Activated, None)
    }

    fn route(&self) -> Arc<FakePrep> {
        self.route.clone().expect("a bound prep route")
    }

    fn spawns(&self) -> usize {
        match &self.route {
            Some(route) => route.spawns(),
            None => 0,
        }
    }

    fn specs(&self) -> Vec<DependencyPrepSpec> {
        match &self.route {
            Some(route) => route.specs(),
            None => Vec::new(),
        }
    }

    fn shells(&self) -> Vec<String> {
        self.shell.fired()
    }
}

fn service_over(shell: Arc<ShellTripwire>, kind: DependencyPrepKind) -> ExecutionService {
    let mut authorization = AuthorizationService::new();
    install_profile_capability(
        &mut authorization,
        &ProfileSource {
            harness_id: "host".into(),
            package_digest: "builtin".into(),
            profile_name: "dependency-preparation".into(),
        },
        vec![kind.executable().to_string()],
        None,
        false,
        vec![],
    );
    ExecutionService::with_backend(
        shell as Arc<dyn CommandExecutionBackend>,
        Arc::new(authorization),
    )
}

/// The service shape production would use: a prep profile covering exactly
/// this toolchain's executable, and the gate bound — never defaulted.
async fn prep_service(harness: &Harness, kind: DependencyPrepKind) -> ExecutionService {
    let service = service_over(Arc::clone(&harness.shell), kind);
    service
        .select_prep_gate(Some(Arc::clone(&harness.gate)))
        .await;
    service
}

/// A required-checks runner whose gate is Activated over the real local shell:
/// the seam that lets a Check actually execute in this suite.
fn offline_runner() -> VerificationRunner {
    let native: Arc<dyn CommandExecutionBackend> = Arc::new(LocalShellBackend::new());
    VerificationRunner::with_backend(Arc::new(SandboxedCheckBackend::new(
        CheckSandboxGate::Activated,
        Some(native),
    )))
}

/// Run one Check over `prep`'s candidate into a fresh private tree.
async fn run_check(runner: &VerificationRunner, prep: &Prep, check_dir: &Path) -> CheckOutcome {
    runner
        .run(
            &prep.binding,
            &prep.manifest,
            &prep.controls(),
            &prep.definition,
            check_dir,
            Duration::from_secs(60),
        )
        .await
}

/// Every file below a directory, relative and forward-slashed, sorted.
fn tree_files(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            out.push(slash(path.strip_prefix(dir).unwrap_or(&path)));
        }
    }
    out.sort();
    out
}

fn digest_entries(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(dir).expect("a readable tree").flatten() {
        let path = entry.path();
        if path.is_dir() {
            digest_entries(root, &path, out);
            continue;
        }
        let relative = slash(path.strip_prefix(root).unwrap_or(&path));
        if relative == VALID_MARKER || relative == POISON_MARKER {
            continue;
        }
        let bytes = std::fs::read(&path).expect("readable bytes");
        out.push((relative, sha256_hex(&bytes)));
    }
}

/// The digest `prepare_dependencies` records, recomputed from the promoted
/// bytes, so a warm read is proven to describe the tree it names.
fn digest_of_tree(dir: &Path, identity: &str) -> String {
    let mut files = Vec::new();
    digest_entries(dir, dir, &mut files);
    files.sort();
    canonical_input_hash(&serde_json::json!({ "identity": identity, "files": files }))
}

/// The permissions a preparation of `network` may carry: task authority never
/// implies a fetch, so network permission follows the requested ceiling.
fn permissions(network: NetworkCeiling) -> EffectivePermissions {
    EffectivePermissions {
        ceiling: PermissionCeiling::Full,
        allow_processes: true,
        allow_network: !network.is_offline(),
    }
}

fn profile(name: &str, executables: &[&str]) -> AuthorizationService {
    let mut authorization = AuthorizationService::new();
    install_profile_capability(
        &mut authorization,
        &ProfileSource {
            harness_id: "host".into(),
            package_digest: "builtin".into(),
            profile_name: name.into(),
        },
        owned(executables),
        None,
        false,
        vec![],
    );
    authorization
}

/// The exact approval that carries a public-internet fetch in v1.
fn dependency_approval() -> PrepNetworkAuthority {
    PrepNetworkAuthority::Approved {
        effect_class: WorkUnitEffectClass::DependencyPreparation,
        ceiling: NetworkCeiling::PublicInternetClient,
    }
}

/// One preparation that really runs and really publishes, returning the
/// harness so callers keep their spawn count and the promoted slot.
async fn cold_prep(prep: &Prep, request: &PreparationRequest) -> (Arc<FakePrep>, PathBuf) {
    let harness = Harness::activated("prep-bytes.js");
    let outcome = prep
        .prepare(&harness, DependencyKind::Cargo, request)
        .await
        .expect("a cold identity prepares, validates and publishes");
    let PreparationOutcome::Cold {
        slot,
        lower_cache_readonly,
    } = outcome
    else {
        panic!("a fresh identity cannot answer warm: {outcome:?}");
    };
    assert!(
        !slot.starts_with(&lower_cache_readonly),
        "the run mounted the slot it was about to write"
    );
    assert_eq!(harness.spawns(), 1);
    assert_eq!(harness.shells(), Vec::<String>::new());
    assert_eq!(prep.state(), CacheSlotState::Warm);
    (harness.route(), slot)
}

/// P21.2 sits between a preparation and candidate lifecycle code: a downgraded
/// argument set must be refused by name, or `npm install` would run arbitrary
/// package scripts inside a run the host later credits.
#[test]
fn a_downgraded_preparation_is_refused_by_its_missing_argument() {
    let temp = tempfile::tempdir().expect("tempdir");
    for (kind, arguments, missing) in [
        (DependencyPrepKind::Cargo, vec!["build"], "fetch"),
        (DependencyPrepKind::Cargo, vec!["fetch"], "--locked"),
        (DependencyPrepKind::Cargo, vec!["--locked"], "fetch"),
        (DependencyPrepKind::Npm, vec!["install"], "ci"),
        (DependencyPrepKind::Npm, vec!["ci"], "--ignore-scripts"),
    ] {
        let error = spec_at(temp.path(), kind, &arguments, NetworkCeiling::Offline, &[])
            .expect_err("a downgraded argument set is a refusal");
        assert!(
            matches!(&error, CheckSpecError::PrepMissingArgument(actual, argument)
                if actual.as_str() == kind.as_str() && argument.as_str() == missing),
            "{kind:?} {arguments:?}: {error}"
        );
        assert!(
            error.to_string().contains("must carry the argument"),
            "{error}"
        );
    }

    // A quoted argument is refused too: the line the route forwards verbatim
    // must stay one argv element per declared argument.
    let quoted = spec_at(
        temp.path(),
        DependencyPrepKind::Cargo,
        &[CARGO_ARGS[0], CARGO_ARGS[1], "--config=\"x\""],
        NetworkCeiling::Offline,
        &[],
    )
    .expect_err("a quote cannot be smuggled into an argument");
    assert!(
        matches!(quoted, CheckSpecError::Reinterpretable(_)),
        "{quoted}"
    );

    let cargo = prep_material(temp.path(), DependencyPrepKind::Cargo, CARGO_ARGS, &[]);
    assert_eq!(cargo.executable, PathBuf::from("cargo"));
    assert_eq!(cargo.command_line(), "cargo fetch --locked");
    assert_eq!(cargo.kind, DependencyPrepKind::Cargo);
    let npm = prep_material(temp.path(), DependencyPrepKind::Npm, NPM_ARGS, &[]);
    assert_eq!(npm.command_line(), "npm ci --ignore-scripts");
    assert_eq!(npm.executable, PathBuf::from("npm"));
    assert_eq!(DependencyPrepKind::Cargo.required_arguments(), CARGO_ARGS);
    assert_eq!(DependencyPrepKind::Npm.required_arguments(), NPM_ARGS);
    assert_eq!(DependencyPrepKind::Npm.executable(), "npm");
    assert_eq!(DependencyPrepKind::Cargo.as_str(), "cargo");
}

/// Acceptance ② at the freeze boundary: an environment key naming a provider
/// credential must be refused with the key in the text, or a typo in a profile
/// would put an API token in a process that fetches over the internet.
#[test]
fn a_provider_credential_key_is_refused_and_named() {
    let temp = tempfile::tempdir().expect("tempdir");
    for key in [
        "OPENAI_API_KEY",
        "apikey",
        "GH_TOKEN",
        "DB_PASSWORD",
        "client_secret",
        "Authorization",
        "AWS_PROVIDER_CONFIG",
        "AWS_SECRET_ACCESS_KEY",
        "ANTHROPIC_API_KEY",
        "R_CODE_PROVIDER_TOKEN",
        "SESSION_COOKIE",
        "XFN_CREDENTIAL",
        "KEY",
        "Anthropic_API_KEY",
    ] {
        let error = spec_at(
            temp.path(),
            DependencyPrepKind::Npm,
            NPM_ARGS,
            NetworkCeiling::Offline,
            &[key],
        )
        .expect_err("a credential key must be refused");
        assert!(
            matches!(&error, CheckSpecError::PrepCredentialEnvironmentKey(actual)
                if actual.as_str() == key),
            "{key}: {error}"
        );
        assert!(error.to_string().contains("provider credential"), "{error}");
        assert!(
            error.to_string().contains(key),
            "the text names it: {error}"
        );
    }

    let cleared = prep_material(temp.path(), DependencyPrepKind::Npm, NPM_ARGS, &[]);
    assert!(cleared.environment_keys.is_empty());
    let inherited = prep_material(
        temp.path(),
        DependencyPrepKind::Npm,
        NPM_ARGS,
        &["PATH", "HOME", "CARGO_HOME", "npm_config_cache"],
    );
    assert_eq!(inherited.environment_keys[3], "npm_config_cache");
}

/// The allowlist is names, never values, and it is the only environment a
/// preparation can express at all. If the frozen material grew a "pass
/// everything through" field, the credential refusals above would stop
/// meaning anything.
#[test]
fn the_environment_allowlist_is_the_only_environment_a_prep_run_carries() {
    let source = include_str!("../src/services/execution.rs");
    let start = source
        .find("pub struct DependencyPrepSpec {")
        .expect("the frozen preparation material");
    let after = &source[start..];
    let end = after.find("\n}").expect("the struct closes");
    let body = &after[..end];
    assert!(body.contains("environment_keys: Vec<String>"), "{body}");
    for escape_hatch in ["HashMap", "env:", "environment:", "inherit", "passthrough"] {
        assert!(
            !body.contains(escape_hatch),
            "the prep material grew an environment escape hatch ({escape_hatch}): {body}"
        );
    }
    assert_eq!(
        body.lines()
            .filter(|line| line.starts_with("    pub "))
            .count(),
        10,
        "every field of the prep material is public and re-validated at the gate"
    );

    let temp = tempfile::tempdir().expect("tempdir");
    let spec = prep_material(
        temp.path(),
        DependencyPrepKind::Cargo,
        CARGO_ARGS,
        &["PATH"],
    );
    let debug = format!("{spec:?}");
    assert!(debug.contains("environment_keys: [\"PATH\"]"), "{debug}");
    let path_value = std::env::var("PATH").unwrap_or_default();
    if !path_value.is_empty() {
        assert!(
            !debug.contains(&path_value),
            "the frozen material carries an environment value, not just a name"
        );
    }
}

/// The three layers are what makes promotion safe at all: a relative or
/// escaping layer, or a missing toolchain identity, would let prepared bytes
/// be read back later as candidate content.
#[test]
fn the_cache_layers_are_admitted_at_construction() {
    let temp = tempfile::tempdir().expect("tempdir");
    let absolute = prep_material(temp.path(), DependencyPrepKind::Cargo, CARGO_ARGS, &[]);
    assert!(absolute.admits_spawn().is_ok());
    assert!(absolute.overlay.starts_with(&absolute.cwd));
    assert!(!absolute.promoted_cache.starts_with(&absolute.overlay));

    let relative_tree = DependencyPrepSpec::build(
        DependencyPrepKind::Cargo,
        &owned(CARGO_ARGS),
        Path::new("tree"),
        TOOLCHAIN,
        NetworkCeiling::Offline,
        &layers(temp.path()),
        &[],
    )
    .expect_err("a relative tree cannot be confined");
    assert!(matches!(relative_tree, CheckSpecError::CwdNotAbsolute(_)));

    for index in 0..3 {
        let mut paths = layers(temp.path());
        let key = match index {
            0 => &mut paths.lower_cache_readonly,
            1 => &mut paths.overlay,
            _ => &mut paths.promoted_cache,
        };
        *key = PathBuf::from("relative/cache/layer");
        let error = DependencyPrepSpec::build(
            DependencyPrepKind::Cargo,
            &owned(CARGO_ARGS),
            &temp.path().join("tree"),
            TOOLCHAIN,
            NetworkCeiling::Offline,
            &paths,
            &[],
        )
        .expect_err("a relative cache layer cannot be confined");
        assert!(
            matches!(&error, CheckSpecError::PrepPathNotAbsolute(path)
                if path.as_str() == "relative/cache/layer"),
            "layer {index}: {error}"
        );
    }

    let anonymous = DependencyPrepSpec::build(
        DependencyPrepKind::Cargo,
        &owned(CARGO_ARGS),
        &temp.path().join("tree"),
        "   ",
        NetworkCeiling::Offline,
        &layers(temp.path()),
        &[],
    )
    .expect_err("evidence must bind a declared toolchain");
    assert!(matches!(anonymous, CheckSpecError::EmptyToolchain));
}

/// P21.1 / P21.3 layout: an overlay outside the run, a slot inside the
/// overlay, a package tree in the cache or a git-visible layer each turn one
/// of the invariants into a lie, so all of them refuse before a spawn exists.
#[test]
fn an_escaping_layout_or_an_unsupported_ceiling_is_refused() {
    let temp = tempfile::tempdir().expect("tempdir");
    let tree = temp.path().join("tree");
    let arguments = owned(CARGO_ARGS);
    let build = |paths: PrepCachePaths| {
        DependencyPrepSpec::build(
            DependencyPrepKind::Cargo,
            &arguments,
            &tree,
            TOOLCHAIN,
            NetworkCeiling::Offline,
            &paths,
            &[],
        )
    };

    let host = spec_at(
        temp.path(),
        DependencyPrepKind::Cargo,
        CARGO_ARGS,
        NetworkCeiling::HostNetwork,
        &[],
    )
    .expect_err("host-network is unsupported in v1, not one approval away");
    assert!(matches!(host, CheckSpecError::PrepNetworkUnsupported));

    let escaping = PrepCachePaths {
        overlay: temp.path().join("elsewhere/overlay"),
        ..layers(temp.path())
    };
    let error = build(escaping).expect_err("the overlay is run-owned");
    assert!(matches!(error, CheckSpecError::PrepOverlayOutsideCwd(_, _)));

    let shared = PrepCachePaths {
        overlay: tree.clone(),
        ..layers(temp.path())
    };
    let error = build(shared).expect_err("the overlay cannot be the whole tree");
    assert!(matches!(error, CheckSpecError::PrepOverlayOutsideCwd(_, _)));

    let inside = PrepCachePaths {
        promoted_cache: temp.path().join("tree/.prep-overlay/promoted"),
        ..layers(temp.path())
    };
    let error = build(inside).expect_err("a slot inside the overlay dies with the run");
    assert!(matches!(
        error,
        CheckSpecError::PrepPromotedInsideOverlay(_, _)
    ));

    let package_tree = PrepCachePaths {
        promoted_cache: temp.path().join("cache/promoted/node_modules"),
        ..layers(temp.path())
    };
    let error = DependencyPrepSpec::build(
        DependencyPrepKind::Npm,
        &owned(NPM_ARGS),
        &tree,
        TOOLCHAIN,
        NetworkCeiling::Offline,
        &package_tree,
        &[],
    )
    .expect_err("a package tree never becomes cache bytes");
    assert!(matches!(error, CheckSpecError::PrepPromotedNodeModules(_)));

    for git in [
        "cache/lower/.git",
        "tree/.prep-overlay/.git",
        "cache/.GIT/promoted",
    ] {
        let mut paths = layers(temp.path());
        if git.contains("lower") {
            paths.lower_cache_readonly = temp.path().join(git);
        } else if git.contains("prep-overlay") {
            paths.overlay = temp.path().join(git);
        } else {
            paths.promoted_cache = temp.path().join(git);
        }
        let error = build(paths).expect_err("INV-06: the git directory stays invisible");
        assert!(
            matches!(error, CheckSpecError::PrepVisibleGit(_)),
            "{git}: {error}"
        );
    }
}

/// One frozen field changed by `edit`, so the tables below vary exactly one
/// thing at a time.
fn variant(base: &DependencyPrepSpec, edit: fn(&mut DependencyPrepSpec)) -> DependencyPrepSpec {
    let mut changed = base.clone();
    edit(&mut changed);
    changed
}

/// Every field of the run is digest-bound and re-admitted at the spawn
/// boundary, because all of them are public: a widened ceiling, an added
/// environment key or a moved overlay is a different run, not one run doing more.
#[test]
fn the_digest_and_the_spawn_gate_bind_every_frozen_field() {
    let temp = tempfile::tempdir().expect("tempdir");
    let base = prep_material(
        temp.path(),
        DependencyPrepKind::Cargo,
        CARGO_ARGS,
        &["PATH"],
    );
    assert_eq!(base.digest().len(), 64);
    assert!(base.digest().bytes().all(|byte| byte.is_ascii_hexdigit()));
    let twin = prep_material(
        temp.path(),
        DependencyPrepKind::Cargo,
        CARGO_ARGS,
        &["PATH"],
    );
    assert_eq!(base.digest(), twin.digest());

    let edits: [SpecEdit; 9] = [
        ("network", |s| {
            s.network = NetworkCeiling::PublicInternetClient
        }),
        ("toolchain", |s| s.toolchain = "cargo-other".into()),
        ("arguments", |s| {
            s.arguments = owned(&["fetch", "--locked", "--offline"])
        }),
        ("cwd", |s| s.cwd = PathBuf::from("/other/tree")),
        ("lower", |s| {
            s.lower_cache_readonly = PathBuf::from("/other/lower")
        }),
        ("overlay", |s| {
            s.overlay = PathBuf::from("/other/tree/.prep-overlay")
        }),
        ("promoted", |s| {
            s.promoted_cache = PathBuf::from("/other/promoted")
        }),
        ("environment", |s| {
            s.environment_keys = owned(&["PATH", "HOME"])
        }),
        ("kind", |s| s.kind = DependencyPrepKind::Npm),
    ];
    for (name, edit) in edits {
        assert_ne!(
            base.digest(),
            variant(&base, edit).digest(),
            "{name} is not digest-bound"
        );
    }
    assert_eq!(
        base.digest(),
        twin.digest(),
        "the base material is unchanged"
    );

    let refusals: [SpecEdit; 5] = [
        ("PrepMissingArgument", |s| s.arguments = owned(&["build"])),
        ("PrepCredentialEnvironmentKey", |s| {
            s.environment_keys.push("API_KEY".into())
        }),
        ("PrepOverlayOutsideCwd", |s| {
            s.overlay = std::env::temp_dir().join("elsewhere")
        }),
        ("PrepNetworkUnsupported", |s| {
            s.network = NetworkCeiling::HostNetwork
        }),
        ("PrepProgramMismatch", |s| {
            s.executable = PathBuf::from("cargo-mitm")
        }),
    ];
    for (name, edit) in refusals {
        let error = variant(&base, edit).admits_spawn().expect_err(name);
        assert!(
            format!("{error:?}").starts_with(name),
            "{name} is not re-admitted at the spawn boundary: {error:?}"
        );
    }
}

/// Acceptance ① / INV-07 at the gate: a closed gate must refuse even with a
/// working route bound behind it, and the caller must see the gate's own text.
/// The control arm proves the footprint is real, so the absence asserted first
/// is not vacuous.
#[tokio::test]
async fn a_closed_prep_gate_spawns_nothing_and_refuses_verbatim() {
    let prep = Prep::new("gate");
    let harness = Harness::closed("prep-bytes.js");
    assert_eq!(harness.gate.route_id(), "sandbox-prep-gated");
    let error = prep
        .prepare_cargo(&harness, &["PATH"])
        .await
        .expect_err("a closed gate has no local route");
    assert!(
        matches!(&error, MaterializeError::PreparationFailed(reason)
            if reason.contains(CLOSED)),
        "the gate's refusal must survive verbatim: {error}"
    );
    assert!(
        error
            .to_string()
            .starts_with("dependency preparation failed: backend failure:"),
        "the only wrapping a closed gate may gain is the route's own label: {error}"
    );
    assert_eq!(harness.spawns(), 0, "the gate forwarded a spawn");
    assert_eq!(harness.shells(), Vec::<String>::new());
    assert!(
        !harness.route().sentinel(&prep.dir).exists(),
        "the bound route ran over a closed gate"
    );
    assert!(
        tree_files(prep.overlay()).is_empty(),
        "a refused preparation deposited bytes in the overlay"
    );
    assert!(!prep.slot().exists(), "nothing was promoted");
    assert_eq!(
        prep.state(),
        CacheSlotState::Cold,
        "a refusal is not a poison"
    );

    let (control, slot) = cold_prep(&prep, &prep.offline(DependencyKind::Cargo, &["PATH"])).await;
    assert!(
        control.sentinel(&prep.dir).is_file(),
        "the control arm never ran, so the absence above proved nothing"
    );
    assert_eq!(control.spawns(), 1);
    assert_eq!(tree_files(&slot), vec![VALID_MARKER, "registry/dep.bin"]);
    assert!(
        !prep.overlay().exists(),
        "the control arm's overlay survived publication"
    );
}

/// INV-07's second layer: an Activated verdict with no route bound is still a
/// refusal, and it must not resolve to any shell-shaped backend — including one
/// a caller bound to the service for everything else. The route id an unbound
/// gate advertises is pinned here because that string is what an operator reads
/// as "preparation is available".
#[tokio::test]
async fn an_activated_gate_with_no_route_never_reaches_a_shell() {
    let harness = Harness::unbound();
    assert_eq!(harness.gate.route_id(), "sandbox-prep-unbound");
    let prep = Prep::new("unbound-route");
    let spec = prep.spec(NetworkCeiling::Offline, &["PATH"]);

    let error = harness
        .gate
        .execute(&spec, None, Duration::from_secs(5))
        .await
        .expect_err("an unbound activated gate runs nothing");
    assert!(
        error.to_string().contains("no native sandbox route"),
        "{error}"
    );

    let outcome = prep
        .prepare_cargo(&harness, &[])
        .await
        .expect_err("no native route means no preparation");
    assert!(
        matches!(&outcome, MaterializeError::PreparationFailed(reason)
            if reason.contains("no native sandbox route")),
        "{outcome}"
    );
    assert_eq!(harness.spawns(), 0);
    assert_eq!(
        harness.shells(),
        Vec::<String>::new(),
        "INV-07: an unbound preparation fell through to a shell backend"
    );
    assert!(
        !tree_files(&prep.dir)
            .iter()
            .any(|file| file.starts_with("route-")),
        "something ran into the private tree without a bound route"
    );
    assert!(tree_files(prep.overlay()).is_empty());
    assert_eq!(prep.state(), CacheSlotState::Cold);

    let closed = SandboxedPrepBackend::closed(CLOSED);
    assert_eq!(closed.route_id(), "sandbox-prep-gated");
    let reason = closed
        .execute(&spec, None, Duration::from_secs(5))
        .await
        .expect_err("a closed gate runs nothing");
    assert_eq!(reason.to_string(), CLOSED);
    let bound = SandboxedPrepBackend::new(
        PrepGate::Activated,
        Some(Arc::new(FakePrep::new("prep-bytes.js", false))),
    );
    assert_eq!(bound.route_id(), "sandbox-prep-native");
}

/// A fetch has no default route: an `ExecutionService` with nothing bound
/// refuses before authorization can matter, and says so through the same id
/// surface an operator reads.
#[tokio::test]
async fn an_unbound_service_reports_prep_unbound_and_refuses_a_fetch() {
    let prep = Prep::new("service-unbound");
    let shell = Arc::new(ShellTripwire::default());
    let service = service_over(Arc::clone(&shell), DependencyPrepKind::Cargo);
    assert_eq!(service.prep_route_id().await, "prep-unbound");
    let error = service
        .run_prep(
            &prep.spec(NetworkCeiling::Offline, &["PATH"]),
            &prep.descriptor(PrepNetworkAuthority::Offline),
            &prep.workspace(),
            &permissions(NetworkCeiling::Offline),
            Duration::from_secs(60),
        )
        .await
        .expect_err("an unbound service has no route for a fetch");
    assert!(matches!(error, ExecutionError::Backend(_)), "{error}");
    assert!(
        error
            .to_string()
            .contains("no dependency-preparation gate is bound"),
        "{error}"
    );
    assert_eq!(shell.fired(), Vec::<String>::new());
    assert_eq!(prep.state(), CacheSlotState::Cold);
    drop(service);

    for (harness, expected) in [
        (Harness::activated("prep-bytes.js"), "sandbox-prep-native"),
        (Harness::unbound(), "sandbox-prep-unbound"),
        (Harness::closed("prep-bytes.js"), "sandbox-prep-gated"),
    ] {
        let service = prep_service(&harness, DependencyPrepKind::Cargo).await;
        assert_eq!(service.prep_route_id().await, expected);
    }
}

/// Defect 3 was that a preparation could only ever be described as a command
/// line. The route is now the only consumer of the whole frozen material, so
/// every mount, the ceiling, the toolchain and the allowlist have to arrive at
/// it verbatim — or P21.1's "mount" and acceptance ② stay unenforceable.
#[tokio::test]
async fn the_bound_route_is_handed_the_whole_frozen_prep_material() {
    let prep = Prep::new("route-material");
    // The layers are resolved before the run: once the slot is published, the
    // identity's own lower layer *is* that slot.
    let expected_lower = prep.cache.lower(prep.identity()).expect("cold lower");
    let expected = prep.spec(NetworkCeiling::PublicInternetClient, &["PATH", "HOME"]);
    let harness = Harness::activated("prep-bytes.js");
    let request = PreparationRequest {
        arguments: PreparationRequest::required_arguments(DependencyKind::Cargo),
        network: NetworkCeiling::PublicInternetClient,
        authority: dependency_approval(),
        environment_keys: owned(&["PATH", "HOME"]),
        timeout: Duration::from_millis(45_000),
    };
    let outcome = prep
        .prepare(&harness, DependencyKind::Cargo, &request)
        .await
        .expect("an approved fetch reaches the bound route");
    let PreparationOutcome::Cold { slot, .. } = &outcome else {
        panic!("an approved fetch must prepare and publish: {outcome:?}");
    };

    let specs = harness.specs();
    assert_eq!(specs.len(), 1, "the route ran exactly once");
    let spec = &specs[0];
    assert_eq!(spec.lower_cache_readonly, expected_lower);
    assert_eq!(spec.overlay, prep.overlay());
    assert_eq!(spec.promoted_cache, prep.slot());
    assert_eq!(spec.cwd, prep.dir);
    assert_eq!(spec.toolchain, TOOLCHAIN);
    assert_eq!(spec.kind, DependencyPrepKind::Cargo);
    assert_eq!(spec.arguments, owned(CARGO_ARGS));
    assert_eq!(spec.executable, PathBuf::from("cargo"));
    assert_eq!(spec.environment_keys, owned(&["PATH", "HOME"]));
    assert_eq!(spec.network, NetworkCeiling::PublicInternetClient);
    assert_eq!(
        spec, &expected,
        "the route ran material the caller cannot account for"
    );
    assert_eq!(
        harness.route().aborts(),
        vec![true],
        "no abort flag reached the route"
    );
    assert_eq!(harness.route().timeouts(), vec![45_000]);
    assert_eq!(harness.shells(), Vec::<String>::new());
    assert_eq!(
        tree_files(slot),
        vec![VALID_MARKER.to_string(), "registry/dep.bin".to_string()],
        "what the route wrote into the overlay it was mounted with is what landed"
    );
    assert!(
        !prep.overlay().exists(),
        "the run-owned overlay survived promotion"
    );
}

/// A `DependencyPrepSpec` has public fields, so the gate — not only the
/// constructor — has to re-admit the material: a spec widened after freezing
/// must never reach a route, or every refusal above is advisory.
#[tokio::test]
async fn no_route_is_ever_handed_a_credential_bearing_spec() {
    let prep = Prep::new("route-refusal");
    let harness = Harness::activated("prep-bytes.js");
    let mut widened = prep.spec(NetworkCeiling::Offline, &["PATH"]);
    widened
        .environment_keys
        .push("R_CODE_PROVIDER_TOKEN".to_string());
    assert!(matches!(
        widened.admits_spawn(),
        Err(CheckSpecError::PrepCredentialEnvironmentKey(_))
    ));
    let error = harness
        .gate
        .execute(&widened, None, Duration::from_secs(5))
        .await
        .expect_err("the gate re-validates before delegating");
    assert!(
        error.to_string().contains("preparation material refused"),
        "{error}"
    );
    assert!(
        error.to_string().contains("provider credential"),
        "the refusal names what is wrong: {error}"
    );

    let service = prep_service(&harness, DependencyPrepKind::Cargo).await;
    let error = service
        .run_prep(
            &widened,
            &prep.descriptor(PrepNetworkAuthority::Offline),
            &prep.workspace(),
            &permissions(NetworkCeiling::Offline),
            Duration::from_secs(5),
        )
        .await
        .expect_err("a widened spec is refused before a route exists");
    assert!(matches!(error, ExecutionError::Spec(_)), "{error}");
    assert_eq!(harness.spawns(), 0);
    assert_eq!(harness.shells(), Vec::<String>::new());
    assert!(tree_files(prep.overlay()).is_empty());
    assert_eq!(prep.state(), CacheSlotState::Cold);
}

/// Acceptance ② at the process boundary: the allowlist the spec carries is
/// enough to build a credential-free environment, and a route that honours it
/// leaves no host provider token in the child. The control arm runs the same
/// tool over the same spec with the allowlist ignored, so "absent" below is
/// proven rather than assumed.
#[tokio::test]
async fn a_route_that_honours_the_allowlist_leaves_no_provider_credential() {
    std::env::set_var(PROVIDER_TOKEN, "host-side-provider-secret");
    let secret = std::env::var(PROVIDER_TOKEN).expect("the marker was set");
    let strict = Prep::with_toolchain("strict-env", "version = 3\nstrict\n", TOOLCHAIN);
    let harness = Harness::filtered("prep-bytes.js");
    let outcome = strict
        .prepare_cargo(&harness, &host_base_keys())
        .await
        .expect("an allowlisted preparation runs");
    assert!(matches!(outcome, PreparationOutcome::Cold { .. }));
    let child_saw = harness.route().stdout();
    assert!(
        child_saw.contains("token=absent"),
        "the allowlisted child still saw a credential: {child_saw}"
    );

    let loose = Prep::with_toolchain("loose-env", "version = 3\nloose\n", TOOLCHAIN);
    let control = Harness::activated("prep-bytes.js");
    let outcome = loose
        .prepare_cargo(&control, &host_base_keys())
        .await
        .expect("the control preparation runs");
    assert!(matches!(outcome, PreparationOutcome::Cold { .. }));
    let control_saw = control.route().stdout();
    assert!(
        control_saw.contains(&format!("token={secret}")),
        "the control arm is not live, so the absence above proved nothing: {control_saw}"
    );
    std::env::remove_var(PROVIDER_TOKEN);
}

/// The network proof is decided before a route exists: an unproven or
/// wrongly-classed ask must be refused while a route that would happily run
/// it is bound, or any route could widen the fetch by re-reading the spec.
#[tokio::test]
async fn an_unproven_fetch_is_refused_before_any_route_exists() {
    let prep = Prep::new("network");
    let harness = Harness::activated("prep-bytes.js");
    let service = prep_service(&harness, DependencyPrepKind::Cargo).await;
    let spec = prep.spec(NetworkCeiling::PublicInternetClient, &[]);
    let workspace = prep.workspace();
    let networked = permissions(NetworkCeiling::PublicInternetClient);

    for authority in [
        PrepNetworkAuthority::Unproven {
            ceiling: NetworkCeiling::PublicInternetClient,
        },
        PrepNetworkAuthority::Approved {
            effect_class: WorkUnitEffectClass::WorkspaceMutation,
            ceiling: NetworkCeiling::PublicInternetClient,
        },
        PrepNetworkAuthority::Offline,
    ] {
        let error = service
            .run_prep(
                &spec,
                &prep.descriptor(authority),
                &workspace,
                &networked,
                Duration::from_secs(60),
            )
            .await
            .expect_err("a networked fetch without the exact proof is refused");
        assert!(matches!(error, ExecutionError::Denied(_)), "{error}");
        assert!(
            error.to_string().contains("exact effect approval"),
            "{authority:?}: {error}"
        );
    }
    assert_eq!(harness.spawns(), 0, "an unproven fetch reached a route");
    assert_eq!(harness.shells(), Vec::<String>::new());
    assert!(tree_files(prep.overlay()).is_empty());

    let output = service
        .run_prep(
            &spec,
            &prep.descriptor(dependency_approval()),
            &workspace,
            &networked,
            Duration::from_secs(60),
        )
        .await
        .expect("the exact approval carries the ask to the gate");
    assert_eq!(output.exit_code, Some(0));
    assert_eq!(harness.spawns(), 1);
    assert_eq!(
        tree_files(prep.overlay()),
        vec!["registry/dep.bin".to_string()]
    );
}

/// Material that stopped being spawnable must be refused as a spec error
/// before authorization and before a route is reached: otherwise a widened
/// field would run on the strength of a decision taken about the old value.
#[tokio::test]
async fn unspawnable_prep_material_is_refused_as_a_spec_error() {
    let prep = Prep::new("spec-refusal");
    let harness = Harness::activated("prep-bytes.js");
    let service = prep_service(&harness, DependencyPrepKind::Cargo).await;
    let workspace = prep.workspace();
    let offline = permissions(NetworkCeiling::Offline);
    let quiet = prep.descriptor(PrepNetworkAuthority::Offline);

    let mut host = prep.spec(NetworkCeiling::Offline, &[]);
    host.network = NetworkCeiling::HostNetwork;
    let error = service
        .run_prep(&host, &quiet, &workspace, &offline, Duration::from_secs(60))
        .await
        .expect_err("an unsupported ceiling never reaches a route");
    assert!(matches!(error, ExecutionError::Spec(_)), "{error}");
    assert!(error.to_string().contains("host-network"), "{error}");

    let mut downgraded = prep.spec(NetworkCeiling::Offline, &[]);
    downgraded.arguments = owned(&["fetch"]);
    let error = service
        .run_prep(
            &downgraded,
            &quiet,
            &workspace,
            &offline,
            Duration::from_secs(60),
        )
        .await
        .expect_err("a preparation without --locked never reaches a route");
    assert!(matches!(error, ExecutionError::Spec(_)), "{error}");
    assert!(error.to_string().contains("--locked"), "{error}");

    let npm = spec_at(
        prep.temp.path(),
        DependencyPrepKind::Npm,
        &["ci"],
        NetworkCeiling::Offline,
        &[],
    )
    .expect_err("an npm ci without --ignore-scripts is never spawnable");
    assert!(
        npm.to_string().contains("--ignore-scripts"),
        "the hook-denial argument must be named in the refusal: {npm}"
    );
    assert_eq!(harness.spawns(), 0);
    assert!(!prep.slot().exists());
    assert_eq!(prep.state(), CacheSlotState::Cold);
}

/// Preparation is the most privileged process v1 runs, so restricted
/// authority, an approval that was never resolved, and an uncovered toolchain
/// each have to stop it before a route is reached.
#[tokio::test]
async fn restricted_authority_and_an_uncovered_executable_fail_closed() {
    let prep = Prep::new("restricted");
    let harness = Harness::activated("prep-bytes.js");
    let service = prep_service(&harness, DependencyPrepKind::Cargo).await;
    let spec = prep.spec(NetworkCeiling::Offline, &[]);
    let quiet = prep.descriptor(PrepNetworkAuthority::Offline);
    let workspace = prep.workspace();

    let read_only = EffectivePermissions::read_only();
    let pending = EffectivePermissions {
        ceiling: PermissionCeiling::ApprovalRequired,
        allow_processes: true,
        allow_network: false,
    };
    for (authority, expected) in [
        (&read_only, "process execution is not allowed"),
        (&pending, "approval required"),
    ] {
        let error = service
            .run_prep(
                &spec,
                &quiet,
                &workspace,
                authority,
                Duration::from_secs(60),
            )
            .await
            .expect_err("restricted authority may not prepare dependencies");
        assert!(
            error.to_string().contains(expected),
            "{authority:?}: {error}"
        );
    }
    assert_eq!(harness.spawns(), 0);

    let uncovered = ExecutionService::with_backend(
        Arc::clone(&harness.shell) as Arc<dyn CommandExecutionBackend>,
        Arc::new(profile("other", &["node"])),
    );
    uncovered
        .select_prep_gate(Some(Arc::clone(&harness.gate)))
        .await;
    let full_offline = permissions(NetworkCeiling::Offline);
    let error = uncovered
        .run_prep(
            &spec,
            &quiet,
            &workspace,
            &full_offline,
            Duration::from_secs(60),
        )
        .await
        .expect_err("an uncovered toolchain fails closed");
    assert!(matches!(error, ExecutionError::Denied(_)), "{error}");
    assert!(
        error.to_string().contains("launch capability"),
        "an uncovered cargo must be refused by name, not by a fallback: {error}"
    );
    assert_eq!(
        harness.spawns(),
        0,
        "an uncovered toolchain reached a bound route"
    );
    assert!(
        !prep.slot().exists() && tree_files(prep.overlay()).is_empty(),
        "a refused preparation left bytes behind"
    );
}

/// Task permission is not a proof: `prepare_dependencies` must refuse a fetch
/// whose authority the runtime did not resolve, and only the exact effect
/// approval may carry a networked preparation to the gate.
#[tokio::test]
async fn prepare_dependencies_refuses_a_fetch_the_authority_does_not_cover() {
    let prep = Prep::new("prep-network");
    let harness = Harness::activated("prep-bytes.js");
    let unproven = PrepNetworkAuthority::Unproven {
        ceiling: NetworkCeiling::PublicInternetClient,
    };
    let error = prep
        .prepare(&harness, DependencyKind::Cargo, &prep.fetching(unproven))
        .await
        .expect_err("task authority is not a proof");
    assert!(
        matches!(&error, MaterializeError::PreparationFailed(reason)
            if reason.contains("exact effect approval")),
        "{error}"
    );
    assert_eq!(harness.spawns(), 0);
    assert_eq!(harness.shells(), Vec::<String>::new());
    assert!(tree_files(prep.overlay()).is_empty());

    let proven = Harness::activated("prep-bytes.js");
    let outcome = prep
        .prepare(
            &proven,
            DependencyKind::Cargo,
            &prep.fetching(dependency_approval()),
        )
        .await
        .expect("the exact approval carries a networked preparation");
    assert!(matches!(outcome, PreparationOutcome::Cold { .. }));
    assert_eq!(proven.spawns(), 1);
    assert_eq!(prep.state(), CacheSlotState::Warm);
}

/// P21.1 / P21.3 privacy of the layers: the overlay is inside the private tree
/// and dies with the run, the slot lives outside it, and promoted bytes never
/// carry a package tree or git metadata a Check could read.
#[tokio::test]
async fn the_overlay_is_run_owned_and_the_slot_lives_outside_the_tree() {
    let prep = Prep::new("layers");
    assert_eq!(prep.overlay(), overlay_path(&prep.dir).as_path());
    assert!(prep.overlay().starts_with(&prep.dir));
    assert_ne!(prep.overlay(), prep.dir.as_path());
    assert!(!prep.prepared.promoted_cache.starts_with(&prep.dir));
    assert!(!prep.prepared.promoted_cache.starts_with(prep.overlay()));
    assert_eq!(
        prep.prepared.promoted_cache,
        prep.slot(),
        "the tree's own default slot is the one its identity owns"
    );
    assert_eq!(
        prep.prepared
            .promoted_cache_in(prep.cache.root())
            .expect("explicit root"),
        prep.slot()
    );
    let cold_lower = prep.cache.lower(prep.identity()).expect("cold lower");
    assert!(!cold_lower.starts_with(prep.slot()));

    let (route, slot) = cold_prep(&prep, &prep.offline(DependencyKind::Cargo, &[])).await;
    assert_eq!(
        route.specs()[0].lower_cache_readonly,
        cold_lower,
        "the run mounted the empty lower layer, not the slot"
    );
    assert!(
        cold_lower.is_dir(),
        "the run never mounted its read-only lower layer"
    );
    assert_ne!(cold_lower, slot, "the two layers are not the same tree");

    assert!(!prep.overlay().exists(), "the overlay outlived the run");
    assert_eq!(tree_files(&slot), vec![VALID_MARKER, "registry/dep.bin"]);
    let still_there = tree_files(&prep.dir);
    assert!(
        !still_there
            .iter()
            .any(|file| file.starts_with(".prep-overlay") || file.ends_with("dep.bin")),
        "an overlay byte stayed in the tree: {still_there:?}"
    );
    assert!(still_there.contains(&"Cargo.lock".to_string()));
    assert_eq!(prep.state(), CacheSlotState::Warm);
    assert_eq!(
        prep.cache.lower(prep.identity()).expect("warm lower"),
        slot,
        "a warmed identity mounts its own promoted slot as the lower layer"
    );
}

/// Defect 1 was that promotion could never land: a cold identity died with
/// `PromotionFailed` and poisoned itself, so `PreparationOutcome::Cold` was
/// unreachable. A fresh root must publish, record the digest it promoted, and
/// answer the next call for the same identity warm without spawning at all.
#[tokio::test]
async fn a_cold_identity_promotes_and_the_next_call_answers_warm() {
    let prep = Prep::new("cold-publish");
    assert_eq!(prep.state(), CacheSlotState::Cold);
    let expected = prep.slot();
    // Resolved before the run: after publication the identity's own lower
    // layer becomes the slot it was just promoted into.
    let cold_lower = prep.cache.lower(prep.identity()).expect("cold lower");
    assert!(!expected.exists());
    let parent = expected.parent().expect("a slot has a parent");
    assert!(
        !parent.exists(),
        "the test tree already grew a promoted layer"
    );

    let first = Harness::activated("prep-bytes.js");
    let outcome = prep
        .prepare_cargo(&first, &["PATH"])
        .await
        .expect("a cold identity prepares and publishes");
    let PreparationOutcome::Cold {
        slot,
        lower_cache_readonly,
    } = outcome
    else {
        panic!("a fresh identity must report Cold: {outcome:?}");
    };
    assert_eq!(slot, prep.slot());
    assert_eq!(first.spawns(), 1);
    assert!(
        first.route().sentinel(&prep.dir).is_file(),
        "the bound route recorded a run it never performed"
    );
    assert!(parent.is_dir(), "the publish never grew its own parent");
    assert_eq!(tree_files(&slot), vec![VALID_MARKER, "registry/dep.bin"]);
    assert_eq!(
        lower_cache_readonly, cold_lower,
        "the run mounted the empty layer, never the slot it wrote"
    );

    let record = std::fs::read_to_string(slot.join(VALID_MARKER)).expect("the validation record");
    let mut lines = record.lines();
    assert_eq!(
        lines.next(),
        Some(prep.identity()),
        "the slot records its own identity"
    );
    assert_eq!(
        lines.next(),
        Some(digest_of_tree(&slot, prep.identity()).as_str()),
        "the recorded digest does not describe the promoted bytes"
    );
    assert_eq!(prep.state(), CacheSlotState::Warm);

    let second = Harness::activated("prep-bytes.js");
    let warm = prep
        .prepare_cargo(&second, &["PATH"])
        .await
        .expect("a validated slot answers warm");
    assert!(
        matches!(warm, PreparationOutcome::Warm { .. }),
        "a published identity must answer warm: {warm:?}"
    );
    assert_eq!(second.spawns(), 0, "a warm hit spawned");
    assert!(
        !second.route().sentinel(&prep.dir).exists(),
        "the second route wrote its sentinel"
    );
    assert_eq!(second.shells(), Vec::<String>::new());
    assert!(
        first.route().sentinel(&prep.dir).is_file(),
        "the run that published left no footprint in its own tree"
    );
    assert_eq!(
        std::fs::read(slot.join("registry/dep.bin")).expect("promoted bytes"),
        b"validated-bytes",
        "a warm identity must describe the bytes it published"
    );
}

/// Defect 2 was that a failed preparation reported a poison it could not
/// record: the marker had nowhere to live, so the identity read Cold again and
/// the next caller re-ran and trusted it. A nonzero exit must leave a durable
/// record under the staging path and refuse the next attempt with zero spawn.
#[tokio::test]
async fn a_failing_preparation_poisons_durably() {
    let prep = Prep::new("failing");
    let harness = Harness::activated("prep-fail.js");
    let error = prep
        .prepare_cargo(&harness, &[])
        .await
        .expect_err("a nonzero exit is not a prepared cache");
    assert!(
        matches!(&error, MaterializeError::CachePoisoned(identity, reason)
            if identity.as_str() == prep.identity()
                && reason.contains("preparation exited with Some(3)")),
        "{error}"
    );
    assert_eq!(harness.spawns(), 1);
    assert!(!prep.slot().exists(), "a failed run published bytes");

    let staging = prep.cache.staging(prep.identity()).expect("staging path");
    assert!(staging.is_dir(), "the poison left no staging record");
    let marker = staging.join(POISON_MARKER);
    assert!(marker.is_file(), "the poison was never recorded");
    let recorded = std::fs::read_to_string(&marker).expect("the poison record");
    assert!(
        recorded.contains("Some(3)"),
        "the record does not name the failure"
    );
    assert_eq!(
        tree_files(&staging),
        owned(&[POISON_MARKER, "registry/dep.bin"]),
        "the failed bytes stayed in the private tree instead of staging"
    );
    assert!(
        !prep.overlay().exists(),
        "unvalidated bytes are still mounted from the run's own tree"
    );
    let state = prep.state();
    assert!(
        matches!(&state, CacheSlotState::Poisoned { reason }
            if reason.contains("preparation exited with Some(3)")),
        "{state:?}"
    );

    let retry = Harness::activated("prep-bytes.js");
    let error = prep
        .prepare_cargo(&retry, &[])
        .await
        .expect_err("a poisoned identity is refused, not re-run and trusted");
    assert!(
        matches!(error, MaterializeError::CachePoisoned(_, _)),
        "{error}"
    );
    assert_eq!(
        retry.spawns(),
        0,
        "the retry spawned over a poisoned identity"
    );
    assert!(!retry.route().sentinel(&prep.dir).exists());
    assert_eq!(
        prep.state(),
        state,
        "the refusal cleaned the poison behind the caller"
    );
}

/// A cold run has to produce bytes and they have to be the validated ones:
/// promoting an empty overlay would let a run that installed nothing be reused
/// forever, and the refusal must leave the identity poisoned, not cold.
#[tokio::test]
async fn a_cold_run_promotes_only_bytes_the_preparation_produced() {
    let prep = Prep::new("cold");
    let harness = Harness::activated("prep-empty.js");
    let error = prep
        .prepare_cargo(&harness, &[])
        .await
        .expect_err("producing nothing is not a cache");
    assert!(
        matches!(&error, MaterializeError::CachePoisoned(identity, reason)
            if identity.as_str() == prep.identity() && reason.contains("no bytes")),
        "{error}"
    );
    assert_eq!(harness.spawns(), 1);
    assert!(!prep.slot().exists(), "an empty staging tree was published");
    assert!(!prep.overlay().exists(), "the overlay must leave the tree");
    let staging = prep.cache.staging(prep.identity()).expect("staging path");
    assert!(staging.is_dir(), "the refused bytes vanished on their own");
    assert!(staging.join(POISON_MARKER).is_file());
    assert!(matches!(prep.state(), CacheSlotState::Poisoned { .. }));

    let retry = Harness::activated("prep-bytes.js");
    let error = prep
        .prepare_cargo(&retry, &[])
        .await
        .expect_err("a poisoned identity is refused, not silently reprepared");
    assert!(
        matches!(error, MaterializeError::CachePoisoned(_, _)),
        "{error}"
    );
    assert_eq!(
        retry.spawns(),
        0,
        "the retry spawned over a poisoned identity"
    );
}

/// P21.1 says a package tree and git metadata stay private. The layout rules
/// cannot see them — they are inside the overlay — so validation of the staged
/// bytes is the only thing that keeps `node_modules` and `.git` out of a slot
/// a later Check mounts read-only.
#[tokio::test]
async fn a_staged_package_tree_or_git_metadata_is_never_promoted() {
    for (mode, script, forbidden) in [
        ("pkg", "prep-pkg.js", "node_modules"),
        ("git", "prep-git.js", ".git"),
    ] {
        let prep = Prep::new(mode);
        let harness = Harness::activated(script);
        let error = prep
            .prepare_cargo(&harness, &[])
            .await
            .expect_err("a private tree byte is not a cache");
        assert!(
            matches!(
                &error,
                MaterializeError::CachePoisoned(_, reason)
                    if reason.contains(forbidden)
                        && reason.contains("must stay inside the private tree")
            ),
            "{mode}: {error}"
        );
        assert_eq!(harness.spawns(), 1);
        assert!(!prep.slot().exists(), "{mode}: a private byte was promoted");
        assert!(
            !prep.overlay().exists(),
            "{mode}: the overlay survived the refusal"
        );
        let staging = prep.cache.staging(prep.identity()).expect("staging path");
        assert!(
            staging.join("registry").join(forbidden).exists(),
            "{mode}: the refused bytes were cleaned up behind the caller"
        );
        assert!(staging.join(POISON_MARKER).is_file());
        assert!(matches!(prep.state(), CacheSlotState::Poisoned { .. }));
    }
}

/// An interrupted promotion must never be observable as a complete cache: a
/// renamed-but-unpublished staging tree is the interruption a caller can build
/// from outside, and it has to read poisoned, refuse reuse, and keep its bytes.
#[tokio::test]
async fn an_interrupted_promotion_never_reads_as_a_complete_cache() {
    let prep = Prep::new("interrupted");
    let staging = prep.cache.staging(prep.identity()).expect("staging path");
    std::fs::create_dir_all(staging.join("registry")).expect("staged tree");
    std::fs::write(staging.join("registry/dep.bin"), b"never-validated").expect("staged bytes");

    let state = prep.state();
    assert!(
        matches!(&state, CacheSlotState::Poisoned { reason }
            if reason.contains("interrupted")),
        "an unpublished staging tree must poison: {state:?}"
    );
    assert!(!prep.slot().exists(), "the identity has no published slot");

    let harness = Harness::activated("prep-bytes.js");
    let error = prep
        .prepare_cargo(&harness, &[])
        .await
        .expect_err("a poisoned identity is never reused as prepared");
    assert!(
        matches!(error, MaterializeError::CachePoisoned(_, _)),
        "{error}"
    );
    assert_eq!(harness.spawns(), 0);
    assert_eq!(
        std::fs::read(staging.join("registry/dep.bin")).expect("staged bytes"),
        b"never-validated",
        "the staged bytes were neither replaced nor promoted"
    );

    std::fs::remove_dir_all(&staging).expect("the caller clears the interruption");
    assert_eq!(prep.state(), CacheSlotState::Cold);
    let (fresh, _slot) = cold_prep(&prep, &prep.offline(DependencyKind::Cargo, &[])).await;
    assert_eq!(fresh.spawns(), 1, "a cleared identity prepares again");
}

/// Acceptance ① in its P21 form: a warm identity answers with no spawn at all.
/// The second call runs behind a gate that would refuse, so a warm hit cannot
/// quietly reach the network, the gate or a host lock.
#[tokio::test]
async fn a_warm_identity_answers_without_touching_gate_lock_or_network() {
    let prep = Prep::new("warm");
    let cold_lower = prep.cache.lower(prep.identity()).expect("cold lower");
    let (first, published) = cold_prep(&prep, &prep.offline(DependencyKind::Cargo, &[])).await;
    assert_eq!(first.spawns(), 1);
    let dependency = published.join("registry/dep.bin");
    let bytes = std::fs::read(&dependency).expect("promoted bytes");

    let harness = Harness::closed("prep-bytes.js");
    let warm = prep
        .prepare(
            &harness,
            DependencyKind::Cargo,
            &prep.fetching(dependency_approval()),
        )
        .await
        .expect("a validated slot answers warm");
    let PreparationOutcome::Warm {
        slot,
        lower_cache_readonly,
    } = warm
    else {
        panic!("a published identity must answer warm: {warm:?}");
    };
    assert_eq!(slot, prep.slot());
    assert_eq!(
        lower_cache_readonly,
        prep.slot(),
        "the lower layer is the slot"
    );
    assert_eq!(published, slot);
    assert_ne!(
        cold_lower, slot,
        "the cold run mounted the empty layer, never the slot it was about to write"
    );
    assert_eq!(harness.spawns(), 0, "a warm hit spawned");
    assert_eq!(harness.shells(), Vec::<String>::new());
    assert!(
        !harness.route().sentinel(&prep.dir).exists(),
        "a warm hit reached the bound route"
    );
    assert_eq!(
        std::fs::read(&dependency).expect("promoted bytes"),
        bytes,
        "reuse must not rewrite the slot"
    );
    let lock = prep.cache.try_lock(prep.identity());
    assert!(lock.is_ok(), "a warm answer must hold no lock");
}

/// A published slot is trusted only while its bytes match its record. Each way
/// of breaking that — edited bytes, a missing record, a record for another
/// identity — must poison with zero spawn, or a stale cache becomes a silent
/// dependency source for every later run.
#[tokio::test]
async fn a_mutated_slot_is_poisoned_and_never_reprepared_in_place() {
    let prep = Prep::new("tampered");
    let (_first, _slot) = cold_prep(&prep, &prep.offline(DependencyKind::Cargo, &[])).await;
    assert_eq!(prep.state(), CacheSlotState::Warm);

    let dependency = prep.slot().join("registry/dep.bin");
    std::fs::write(&dependency, b"tampered-bytes").expect("tamper");
    let state = prep.state();
    assert!(
        matches!(&state, CacheSlotState::Poisoned { reason }
            if reason.contains("no longer matches its digest")),
        "{state:?}"
    );

    let harness = Harness::activated("prep-bytes.js");
    let error = prep
        .prepare_cargo(&harness, &[])
        .await
        .expect_err("tampered cache bytes are refused, not repaired");
    assert!(
        matches!(error, MaterializeError::CachePoisoned(_, _)),
        "{error}"
    );
    assert_eq!(harness.spawns(), 0);
    assert_eq!(
        std::fs::read(&dependency).expect("still tampered"),
        b"tampered-bytes",
        "the refusal overwrote the evidence"
    );

    std::fs::remove_file(prep.slot().join(VALID_MARKER)).expect("drop the record");
    let missing = prep.state();
    assert!(
        matches!(&missing, CacheSlotState::Poisoned { reason }
            if reason.contains("no validation marker")),
        "{missing:?}"
    );
    std::fs::write(
        prep.slot().join(VALID_MARKER),
        format!("{}\n{}\n", "f".repeat(64), "0".repeat(64)),
    )
    .expect("a record for another identity");
    let foreign = prep.state();
    assert!(
        matches!(&foreign, CacheSlotState::Poisoned { reason }
            if reason.contains("another cache identity")),
        "{foreign:?}"
    );
}

/// "Content-addressed" has to mean the identity comes from the dependency
/// inputs and nothing else, and a value that is not a digest must never become
/// a path — or a poisoned name could read or write any directory on the host.
#[test]
fn the_cache_identity_is_content_addressed_and_garbage_is_refused() {
    let first = Prep::with_toolchain("one", "version = 3\nsource-dep\n", TOOLCHAIN);
    let same_lock = Prep::with_toolchain("two", "version = 3\nsource-dep\n", TOOLCHAIN);
    let changed = Prep::with_toolchain("three", "version = 3\ntampered-dep\n", TOOLCHAIN);
    let other_tool = Prep::with_toolchain("four", "version = 3\nsource-dep\n", "cargo-other");

    let identity = first.identity();
    assert_eq!(identity.len(), 64);
    assert!(identity.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_eq!(
        identity,
        same_lock.identity(),
        "the identity binds the dependency inputs, not the tree location"
    );
    assert_ne!(
        identity,
        changed.identity(),
        "lock bytes must move the hash"
    );
    assert_ne!(
        identity,
        other_tool.identity(),
        "the toolchain must move it"
    );
    assert_ne!(
        first.prepared.promoted_cache, changed.prepared.promoted_cache,
        "two dependency sets may not share a slot"
    );

    let long = "a".repeat(63);
    let non_hex = "z".repeat(64);
    let root = tempfile::tempdir().expect("tempdir");
    for garbage in ["", "short", &long, &non_hex, "node_modules", "../escape"] {
        let error = promoted_path_for(root.path(), garbage)
            .expect_err("a non-digest identity is not a path");
        assert!(
            matches!(&error, MaterializeError::CacheIdentity(actual)
                if actual.as_str() == garbage),
            "{garbage}: {error}"
        );
        let cache = DependencyCache::new(root.path()).expect("absolute root");
        assert!(cache.promoted(garbage).is_err(), "{garbage} named a slot");
        assert!(cache.staging(garbage).is_err(), "{garbage} named staging");
        assert!(
            cache.lower(garbage).is_err(),
            "{garbage} named a lower layer"
        );
        assert!(
            matches!(cache.state(garbage), CacheSlotState::Poisoned { .. }),
            "a garbage identity must not read as cold or warm"
        );
    }
    let relative = DependencyCache::new(Path::new("relative/cache-root"))
        .expect_err("a relative root cannot be confined");
    assert!(matches!(
        relative,
        MaterializeError::CacheRootNotAbsolute(_)
    ));
}

/// The host lock is what makes promotion safe against a peer: while one run
/// holds a slot nobody else may prepare it, a lock abandoned by a killed run is
/// never stolen, and two racing callers share exactly one spawn between them.
#[tokio::test]
async fn only_one_preparation_holds_a_slot_at_a_time() {
    let prep = Prep::new("locked");
    let held = prep.cache.try_lock(prep.identity()).expect("first holder");
    let harness = Harness::activated("prep-bytes.js");
    let error = prep
        .prepare_cargo(&harness, &[])
        .await
        .expect_err("a peer holds the slot");
    assert!(
        matches!(&error, MaterializeError::CacheBusy(identity)
            if identity.as_str() == prep.identity()),
        "{error}"
    );
    assert_eq!(harness.spawns(), 0);
    assert_eq!(
        prep.state(),
        CacheSlotState::Cold,
        "a refusal poisoned nothing"
    );
    drop(held);

    let abandoned = prep
        .cache
        .try_lock(&"c".repeat(64))
        .expect("a second identity");
    let _leaked: &'static mut CacheLock = Box::leak(Box::new(abandoned));
    let error = prep
        .cache
        .try_lock(&"c".repeat(64))
        .expect_err("an abandoned lock is never stolen");
    assert!(matches!(error, MaterializeError::CacheBusy(_)), "{error}");

    let racer = Harness::activated("prep-bytes.js");
    let service = prep_service(&racer, DependencyPrepKind::Cargo).await;
    let request = prep.offline(DependencyKind::Cargo, &[]);
    let (first, second) = tokio::join!(
        prepare_dependencies(
            &service,
            &prep.prepared,
            DependencyKind::Cargo,
            &prep.cache,
            &request
        ),
        prepare_dependencies(
            &service,
            &prep.prepared,
            DependencyKind::Cargo,
            &prep.cache,
            &request
        )
    );
    let mut busy = 0;
    let mut published = 0;
    for outcome in [first, second] {
        match outcome {
            Ok(winner) => {
                assert!(
                    matches!(winner, PreparationOutcome::Cold { .. }),
                    "two preparations over a cold identity: {winner:?}"
                );
                published += 1;
            }
            Err(MaterializeError::CacheBusy(_)) => busy += 1,
            Err(error) => panic!("the race ended in an unexpected refusal: {error}"),
        }
    }
    assert_eq!(
        busy, 1,
        "exactly one caller may be refused by the host lock"
    );
    assert_eq!(published, 1, "exactly one caller may publish");
    assert_eq!(
        racer.spawns(),
        1,
        "two preparations spawned twice, so the lock decided nothing"
    );
    assert_eq!(prep.state(), CacheSlotState::Warm);
}

/// Preparation is only reproducible against a preserved lockfile, so a tree
/// missing the kind's lock must be refused before a process exists; the
/// mandatory argument pairs are the public contract that keeps it that way.
#[tokio::test]
async fn a_tree_without_its_declared_lockfile_is_never_prepared() {
    let prep = Prep::new("nolock");
    let harness = Harness::activated("prep-bytes.js");
    let request = PreparationRequest::offline(
        PreparationRequest::required_arguments(DependencyKind::Npm),
        vec![],
        Duration::from_secs(60),
    );
    let error = prepare_dependencies(
        &prep_service(&harness, DependencyPrepKind::Npm).await,
        &prep.prepared,
        DependencyKind::Npm,
        &prep.cache,
        &request,
    )
    .await
    .expect_err("npm has no package-lock.json here");
    assert!(
        matches!(&error, MaterializeError::UndeclaredInput(reason)
            if reason.contains("package-lock.json")),
        "{error}"
    );
    assert_eq!(harness.spawns(), 0);
    assert_eq!(DependencyKind::Cargo.lockfile(), "Cargo.lock");
    assert_eq!(DependencyKind::Npm.lockfile(), "package-lock.json");
    assert_eq!(DependencyKind::Npm.prep_kind(), DependencyPrepKind::Npm);
    assert_eq!(DependencyKind::Cargo.prep_kind(), DependencyPrepKind::Cargo);
    assert_eq!(
        PreparationRequest::required_arguments(DependencyKind::Cargo),
        owned(CARGO_ARGS)
    );
    assert_eq!(request.network, NetworkCeiling::Offline);
    assert_eq!(request.authority, PrepNetworkAuthority::Offline);
    assert!(request.environment_keys.is_empty());
}

/// P21.3 is the half of the contract that turns a cache into something a Check
/// may read: the promoted slot joins the check's read roots, the overlay never
/// does, and a cold identity adds nothing it has not validated.
#[tokio::test]
async fn a_check_mounts_the_promoted_slot_and_never_the_overlay() {
    let prep = Prep::new("check-mount");
    let check_dir = prep.temp.path().join("verify-mounted");
    let cold = CheckSpawnSpec::build(
        "node",
        &owned(&["verify.js"]),
        &check_dir,
        TOOLCHAIN,
        &owned(&["src"]),
    )
    .expect("check material over an unmaterialized tree");
    assert_eq!(
        cold.read_roots,
        vec![slash(&check_dir), slash(&check_dir.join("src"))],
        "a check with no promoted slot must not fake a mount"
    );

    let (_, slot) = cold_prep(&prep, &prep.offline(DependencyKind::Cargo, &[])).await;
    assert_eq!(
        slot,
        prep.prepared
            .promoted_cache_in(prep.cache.root())
            .expect("slot for the same identity"),
        "content addressing makes the check's own slot the published one"
    );
    let overlay = overlay_path(&check_dir);
    let mounted = cold
        .clone()
        .with_promoted_cache(&slot, &overlay)
        .expect("the promoted slot mounts read-only");
    assert_eq!(
        mounted.read_roots,
        vec![
            slash(&check_dir),
            slash(&check_dir.join("src")),
            slash(&slot)
        ],
        "P21.3: the check mounts exactly the promoted slot"
    );
    for root in &mounted.read_roots {
        assert!(
            !root.contains(".prep-overlay"),
            "a Check mounts the overlay: {root}"
        );
        assert!(
            !root.contains(&slash(prep.overlay())),
            "a Check mounts the run overlay"
        );
        assert!(
            !visible_git_root(root),
            "INV-06: a check root exposes .git: {root}"
        );
    }
    assert_eq!(mounted.network, NetworkCeiling::Offline);
    assert!(mounted.admits_spawn().is_ok());
    assert_ne!(
        mounted.digest(),
        cold.digest(),
        "a mounted cache must stale prior evidence, not reuse it"
    );
    let again = mounted
        .clone()
        .with_promoted_cache(&slot, &overlay)
        .expect("mounting the same slot twice");
    assert_eq!(
        again.read_roots, mounted.read_roots,
        "the mount is idempotent"
    );

    let outcome = run_check(&offline_runner(), &prep, &check_dir).await;
    assert_eq!(outcome.status, CheckStatus::Passed, "{outcome:?}");
}

/// Defect 4 was that nothing added the promoted cache to a Check's material.
/// The mount has to reach the evidence the check mints, and only the mount may
/// change it: the same tree re-checked without a slot must fingerprint exactly
/// as it did before, or the proof below is a coincidence.
#[tokio::test]
async fn a_mounted_slot_stales_the_evidence_a_check_without_one_minted() {
    let prep = Prep::new("check-evidence");
    let runner = offline_runner();
    assert_eq!(runner.network_ceiling(), NetworkCeiling::Offline);
    let check_dir = prep.temp.path().join("verify-evidence");

    let first = run_check(&runner, &prep, &check_dir).await;
    assert_eq!(first.status, CheckStatus::Passed, "{first:?}");
    let without = first
        .evidence
        .as_ref()
        .expect("a usable check mints evidence")
        .environment_fingerprint
        .clone();

    std::fs::remove_dir_all(&check_dir).expect("free the private tree");
    let (_route, slot) = cold_prep(&prep, &prep.offline(DependencyKind::Cargo, &[])).await;
    let second = run_check(&runner, &prep, &check_dir).await;
    assert_eq!(second.status, CheckStatus::Passed, "{second:?}");
    let with = second
        .evidence
        .as_ref()
        .expect("evidence")
        .environment_fingerprint
        .clone();
    assert_ne!(
        without, with,
        "the promoted mount never reached the evidence fingerprint"
    );

    std::fs::remove_dir_all(&check_dir).expect("free the private tree again");
    std::fs::remove_dir_all(&slot).expect("the caller drops the cache");
    assert_eq!(prep.state(), CacheSlotState::Cold);
    let third = run_check(&runner, &prep, &check_dir).await;
    assert_eq!(third.status, CheckStatus::Passed, "{third:?}");
    assert_eq!(
        third
            .evidence
            .as_ref()
            .expect("evidence")
            .environment_fingerprint,
        without,
        "a check without a mount must fingerprint as it did before"
    );
}

/// A Check may only ever read a slot the host published: a relative name, a
/// slot nested with the run overlay in either direction, one inside the check's
/// own tree, or one the git directory can reach must refuse rather than widen
/// the read roots — that refusal is what makes acceptance ③ durable.
#[test]
fn a_check_refuses_a_slot_it_cannot_mount_read_only() {
    let temp = tempfile::tempdir().expect("tempdir");
    let check_dir = temp.path().join("verify");
    let overlay = check_dir.join(".prep-overlay");
    let base = CheckSpawnSpec::build(
        "node",
        &owned(&["verify.js"]),
        &check_dir,
        TOOLCHAIN,
        &owned(&["src"]),
    )
    .expect("check material");

    // Matched on the refusal's own variant name, so a mount that slips past one
    // rule cannot be reported as "refused, for some other reason".
    let refuse = |slot: &Path, variant: &str, because: &str| match base
        .clone()
        .with_promoted_cache(slot, &overlay)
    {
        Ok(mounted) => panic!("{because}: mounted {mounted:?}"),
        Err(error) => assert!(
            format!("{error:?}").starts_with(variant),
            "{because}: {error:?}"
        ),
    };
    let git_slot = temp.path().join("cache/.git/objects");
    refuse(
        Path::new("relative/slot"),
        "PrepPathNotAbsolute",
        "a relative slot escapes",
    );
    refuse(
        &overlay.join("promoted"),
        "PrepPromotedInsideOverlay",
        "overlay bytes unpublished",
    );
    refuse(
        temp.path(),
        "PrepPromotedInsideOverlay",
        "the overlay sits in the slot",
    );
    refuse(
        &check_dir.join("deps"),
        "PrepPromotedInsideOverlay",
        "the tree is not a cache",
    );
    refuse(
        &git_slot,
        "ReadRootVisibleGit",
        "INV-06: .git would be visible",
    );
    assert_eq!(
        base.read_roots.len(),
        2,
        "a refused mount must not mutate the spec"
    );
}

/// Acceptance ① with a mount present: a preparation that really fetched over
/// the internet must leave the Check offline, gated, and blind to the overlay.
/// A non-Activated check gate stays a refusal, never a silent pass.
#[tokio::test]
async fn an_approved_networked_prep_leaves_the_check_offline_and_gated() {
    let prep = Prep::new("check-offline");
    let (route, slot) = cold_prep(&prep, &prep.fetching(dependency_approval())).await;
    assert_eq!(route.spawns(), 1);
    assert!(slot.starts_with(prep.cache.root()));

    let check_dir = prep.temp.path().join("verify-ceiling");
    let base = CheckSpawnSpec::build(
        "node",
        &owned(&["verify.js"]),
        &check_dir,
        TOOLCHAIN,
        &owned(&["src"]),
    )
    .expect("check material");
    let mounted = base
        .clone()
        .with_promoted_cache(&slot, &overlay_path(&check_dir))
        .expect("the slot mounts");
    assert_eq!(mounted.network, NetworkCeiling::Offline);
    assert!(!mounted
        .read_roots
        .iter()
        .any(|root| root.contains(&slash(prep.overlay()))));

    let mut smuggled = mounted.clone();
    smuggled.network = NetworkCeiling::PublicInternetClient;
    assert_eq!(
        smuggled.admits_spawn(),
        Err(CheckSpecError::NetworkNotOffline),
        "a widened ceiling on check material is a refusal, not a run"
    );

    for ceiling in [
        VerificationRunner::new().network_ceiling(),
        offline_runner().network_ceiling(),
    ] {
        assert_eq!(ceiling, NetworkCeiling::Offline);
    }
    let gated = VerificationRunner::with_backend(Arc::new(SandboxedCheckBackend::closed(
        "sandbox-disabled: no native checks route",
    )));
    assert_eq!(gated.backend_id(), "sandbox-gated");
    let outcome: CheckOutcome = gated
        .run(
            &prep.binding,
            &prep.manifest,
            &prep.controls(),
            &prep.definition,
            &check_dir,
            Duration::from_secs(60),
        )
        .await;
    assert!(
        matches!(&outcome.status, CheckStatus::Unavailable { reason }
            if reason.contains("sandbox-disabled")),
        "a non-Activated gate must never masquerade as a run: {outcome:?}"
    );
    assert_eq!(outcome.evidence, None);
    assert!(
        !slot.join("verify.js").exists(),
        "the check wrote into the promoted slot"
    );

    let passed = offline_runner()
        .run(
            &prep.binding,
            &prep.manifest,
            &prep.controls(),
            &prep.definition,
            &prep.temp.path().join("verify-passing"),
            Duration::from_secs(60),
        )
        .await;
    assert_eq!(passed.status, CheckStatus::Passed, "{passed:?}");
    assert_eq!(passed.exit_code, Some(0));
}

/// P21's authorization rule, decided in one place: task permissions never
/// imply a fetch. Only an approval for exactly the dependency-preparation
/// effect class at exactly the asked ceiling does, and never anything wider.
#[test]
fn preparation_network_requires_the_exact_effect_approval() {
    let service = profile("dependency-preparation", &["cargo"]);
    let workspace = WorkspaceCapability::WriteWithin {
        root: "D:/work".into(),
    };
    let credentials = CredentialScope::default();
    let public = NetworkCeiling::PublicInternetClient;
    let no_network = permissions(NetworkCeiling::Offline);
    let ask = |authority| {
        OperationDescriptor::dependency_preparation(
            "cargo",
            owned(CARGO_ARGS),
            Some("D:/work".into()),
            authority,
        )
    };
    let decide = |descriptor: &OperationDescriptor, authority: &EffectivePermissions| {
        service.authorize(descriptor, &workspace, authority, &credentials)
    };

    let quiet = ask(PrepNetworkAuthority::Offline);
    assert_eq!(decide(&quiet, &no_network), AuthorizationDecision::Allowed);
    // Network permission the proof does not back stops at approval — even for
    // a run asking nothing, so an offline prep cannot borrow a task's grant.
    assert!(matches!(
        decide(&quiet, &EffectivePermissions::full()),
        AuthorizationDecision::RequiresApproval { .. }
    ));
    let unproven = ask(PrepNetworkAuthority::Unproven { ceiling: public });
    assert!(matches!(
        decide(&unproven, &EffectivePermissions::full()),
        AuthorizationDecision::RequiresApproval { .. }
    ));
    let foreign = ask(PrepNetworkAuthority::Approved {
        effect_class: WorkUnitEffectClass::WorkspaceMutation,
        ceiling: public,
    });
    assert!(matches!(
        decide(&foreign, &EffectivePermissions::full()),
        AuthorizationDecision::RequiresApproval { .. }
    ));
    let proven = ask(dependency_approval());
    assert_eq!(
        decide(&proven, &EffectivePermissions::full()),
        AuthorizationDecision::Allowed
    );
    assert!(matches!(
        decide(&proven, &no_network),
        AuthorizationDecision::Denied(DenyReason::NetworkDisabled)
    ));
    assert!(matches!(
        decide(&proven, &EffectivePermissions::read_only()),
        AuthorizationDecision::Denied(DenyReason::ProcessesDisabled)
    ));

    assert!(proven.prep_network.proves(public));
    assert!(!proven.prep_network.proves(NetworkCeiling::Offline));
    assert!(!proven.prep_network.proves(NetworkCeiling::HostNetwork));
    assert!(proven.prep_network.proves_network());
    assert!(proven.prep_network.asks_network());
    assert!(!quiet.prep_network.asks_network());
    assert!(!quiet.prep_network.proves_network());
    assert!(unproven.prep_network.asks_network());
    assert!(!unproven.prep_network.proves_network());
    let host = ask(PrepNetworkAuthority::Approved {
        effect_class: WorkUnitEffectClass::DependencyPreparation,
        ceiling: NetworkCeiling::HostNetwork,
    });
    assert!(
        !host.prep_network.proves_network(),
        "nothing may prove host-network in v1"
    );
    assert!(!host.prep_network.proves(NetworkCeiling::HostNetwork));

    let refused = decide(&unproven, &EffectivePermissions::full());
    assert!(
        matches!(
            apply_approval_decision(refused.clone(), false),
            AuthorizationDecision::Denied(DenyReason::ApprovalDenied(_))
        ),
        "a refused approval must resolve to a denial"
    );
    assert_eq!(
        apply_approval_decision(refused, true),
        AuthorizationDecision::Allowed
    );
}

/// Defect 3 pinned in source: preparation must not be expressible as a shell
/// command at all, the gate must name its own resolution, and the preparation
/// service may not reach for any other execution entry point.
#[test]
fn the_prep_route_has_no_unsandboxed_fallback() {
    let execution = include_str!("../src/services/execution.rs");
    assert_eq!(
        execution
            .matches("impl CommandExecutionBackend for SandboxedPrepBackend")
            .count(),
        0,
        "preparation is back on the shell-shaped backend it left"
    );
    assert_eq!(
        execution
            .matches("impl CommandExecutionBackend for")
            .count(),
        1,
        "only the required-checks backend may implement the shell shape"
    );
    assert!(
        execution.contains("pub trait DependencyPrepRoute"),
        "the route that receives the whole spec is gone"
    );

    let start = execution
        .find("pub async fn run_prep(")
        .expect("the gated prep entry point");
    let tail = &execution[start..];
    let end = start
        + tail
            .find("\n    async fn execute(")
            .expect("run_prep ends before the shell route");
    let body = &execution[start..end];
    assert!(
        !body.contains("CommandSpec {"),
        "run_prep rebuilds a shell command instead of the frozen spec"
    );
    assert!(
        !body.contains("effective_backend("),
        "run_prep resolves the shell backend chain again"
    );
    assert!(
        body.contains("no dependency-preparation gate is bound"),
        "a fetch may still default to a route: {body}"
    );
    assert!(
        body.contains("gate.execute(") && body.contains("Some(&self.abort)"),
        "the gate no longer receives the whole spec with the abort flag: {body}"
    );

    let block = execution
        .find("impl SandboxedPrepBackend {")
        .expect("the prep gate's own implementation");
    let block = &execution[block..];
    for shell in [
        "LocalShellBackend",
        "CommandExecutionBackend",
        "CommandSpec",
    ] {
        assert!(
            !block.contains(shell),
            "INV-07: the prep gate grew a shell-shaped route ({shell})"
        );
    }
    assert!(
        block.contains("no native sandbox route is bound"),
        "an activated gate with no route may fall through: {block}"
    );

    let service = include_str!("../src/services/verification_inputs.rs");
    for shell in [
        "LocalShellBackend",
        "CommandSpec",
        "run_authorized(",
        "run_check(",
    ] {
        assert!(
            !service.contains(shell),
            "the preparation service may not name {shell}"
        );
    }
    assert!(
        service.contains(".run_prep("),
        "preparation must go through the gated prep entry point"
    );
}

/// Defect 4 pinned in source, and the honest v1 state: the only Check-side
/// effect of P21 is the promoted mount, and nothing in production drives a
/// preparation yet — so a wiring of `prepare_dependencies` must be reviewed as
/// a new effect, not quietly added.
#[test]
fn the_only_check_side_effect_of_p21_is_the_promoted_mount() {
    let verification = include_str!("../src/services/verification.rs");
    let mount = verification
        .find(".with_promoted_cache(&prepared.promoted_cache, &prepared.overlay)")
        .expect("P21.3 mounts the promoted slot into the check's own material");
    let guard = verification
        .find("prepared.promoted_cache.is_dir()")
        .expect("the mount is conditional on a published slot");
    assert!(
        guard < mount,
        "the check may mount a slot that was never published"
    );
    assert!(
        verification.contains("promoted cache cannot be mounted:"),
        "a refused mount must surface as Unavailable, not as a pass"
    );
    assert!(
        !verification.contains(".prep-overlay"),
        "the run overlay must never reach a check's material"
    );

    let mut sources = Vec::new();
    collect_rust_sources(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut sources,
    );
    let mut defined = 0;
    let mut callers = Vec::new();
    for path in sources {
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let hits = content.matches("prepare_dependencies").count();
        if hits > 1 {
            panic!(
                "{:?} names prepare_dependencies {hits} times; a call site may appear once",
                path
            );
        }
        if hits == 1 {
            if path.ends_with("verification_inputs.rs") {
                defined += 1;
            } else {
                callers.push(slash(&path));
            }
        }
    }
    assert_eq!(
        defined, 1,
        "the preparation entry point is defined exactly once"
    );
    assert!(
        callers.is_empty(),
        "prepare_dependencies is still unwired in production, so any new caller \
         is an unreviewed network effect: {callers:?}"
    );
}

/// Every `.rs` file below a directory.
fn collect_rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rust_sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}
