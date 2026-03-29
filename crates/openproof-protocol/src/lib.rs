use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
    System,
    #[default]
    Notice,
    ToolCall,
    ToolResult,
    /// File change with colored diff lines.
    Diff,
    /// Model's reasoning/thinking (shown as dim text).
    Thought,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TranscriptEntry {
    pub id: String,
    pub role: MessageRole,
    pub title: Option<String>,
    pub content: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ToolCallEntry {
    pub call_id: String,
    pub tool_name: String,
    /// JSON-encoded arguments.
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ToolResultEntry {
    pub call_id: String,
    pub tool_name: String,
    pub success: bool,
    pub output: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WorkspaceFileEntry {
    /// Relative path within the session workspace directory.
    pub path: String,
    pub size_bytes: usize,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProofNodeKind {
    #[default]
    Theorem,
    Lemma,
    Artifact,
    Attempt,
    Conjecture,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProofNodeStatus {
    #[default]
    Pending,
    Suggested,
    Proving,
    Verifying,
    Verified,
    Failed,
    Abandoned,
}

/// A timestamped activity event for the dashboard feed.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ActivityEntry {
    pub timestamp: String,
    pub kind: String,
    pub message: String,
}

/// Status of a proof goal in the Pantograph proof tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    #[default]
    Open,
    InProgress,
    Closed,
    Failed,
}

/// A goal in the Pantograph proof tree, visible in the dashboard.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProofGoal {
    pub id: String,
    /// The goal's type expression (what needs to be proved).
    pub goal_text: String,
    pub status: GoalStatus,
    /// Parent goal that was split to create this one.
    pub parent_goal_id: Option<String>,
    /// Tactic that created this subgoal (from splitting the parent).
    pub tactic_applied: Option<String>,
    /// Tactics that were tried and failed on this goal.
    pub failed_tactics: Vec<String>,
    /// Number of tactics attempted on this goal.
    pub attempts: usize,
    /// Line in the source file where this goal's sorry is.
    pub sorry_line: Option<usize>,
    /// Pantograph state ID (for chaining further tactics).
    pub state_id: Option<u64>,
    /// Who closed this goal: "agent:prover", "agent:repairer", etc. None = BFS.
    pub solved_by: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    #[default]
    Planner,
    Prover,
    Repairer,
    Retriever,
    Critic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    #[default]
    Idle,
    Running,
    Blocked,
    Done,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BranchQueueState {
    #[default]
    Queued,
    Running,
    WaitingVerify,
    Blocked,
    Done,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProofQuestionOption {
    pub id: String,
    pub label: String,
    pub summary: String,
    pub formal_target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProofQuestionState {
    pub prompt: String,
    pub options: Vec<ProofQuestionOption>,
    pub recommended_option_id: Option<String>,
    pub answer_text: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProofNode {
    pub id: String,
    pub kind: ProofNodeKind,
    pub label: String,
    pub statement: String,
    pub content: String,
    pub status: ProofNodeStatus,
    /// Parent node ID -- forms the proof tree. Root nodes have None.
    pub parent_id: Option<String>,
    /// IDs of nodes this node depends on (uses in its proof).
    pub depends_on: Vec<String>,
    /// Depth in the proof tree (0 = root theorem).
    pub depth: usize,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct BranchMessage {
    pub id: String,
    pub role: MessageRole,
    pub content: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AgentTask {
    pub id: String,
    pub role: AgentRole,
    pub title: String,
    pub status: AgentStatus,
    pub description: String,
    pub branch_id: Option<String>,
    pub output: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AgentRecord {
    pub id: String,
    pub role: AgentRole,
    pub status: AgentStatus,
    pub title: String,
    pub tasks: Vec<AgentTask>,
    pub current_task_id: Option<String>,
    pub branch_ids: Vec<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProofBranch {
    pub id: String,
    pub role: AgentRole,
    pub title: String,
    pub branch_kind: String,
    pub hidden: bool,
    pub status: AgentStatus,
    pub phase: Option<String>,
    pub queue_state: BranchQueueState,
    pub task_id: Option<String>,
    pub focus_node_id: Option<String>,
    pub goal_summary: String,
    pub score: f32,
    pub attempt_count: usize,
    pub progress_kind: Option<String>,
    pub last_lean_diagnostic: String,
    pub latest_diagnostics: Option<String>,
    pub latest_goals: Option<String>,
    pub last_successful_check_at: Option<String>,
    pub search_status: String,
    pub lean_snippet: String,
    pub diagnostics: String,
    pub summary: String,
    pub promoted_from_hidden: bool,
    pub superseded_by_branch_id: Option<String>,
    pub transcript: Vec<BranchMessage>,
    /// History of BFS search attempts on this branch's focus node.
    pub search_history: Vec<SearchAttemptMetrics>,
    pub created_at: String,
    pub updated_at: String,
}

/// Metrics from a single BFS tactic search attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchAttemptMetrics {
    /// Number of remaining unsolved goals after search.
    pub remaining_goals: usize,
    /// Number of tactic expansions tried.
    pub expansions: usize,
    /// Whether the search timed out (vs exhausted all candidates).
    pub timed_out: bool,
    /// Search outcome: "solved", "partial", "exhausted", "timeout".
    pub outcome: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LeanVerificationSummary {
    pub ok: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub error: Option<String>,
    pub checked_at: String,
    pub project_dir: String,
    pub scratch_path: String,
    pub rendered_scratch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProofSessionState {
    pub phase: String,
    pub status_line: String,
    pub root_node_id: Option<String>,
    pub problem: Option<String>,
    pub formal_target: Option<String>,
    pub accepted_target: Option<String>,
    pub search_status: Option<String>,
    pub assumptions: Vec<String>,
    pub paper_notes: Vec<String>,
    pub pending_question: Option<ProofQuestionState>,
    pub awaiting_clarification: bool,
    pub is_autonomous_running: bool,
    /// Full autonomous = never stop (except when all nodes verified or user interrupts).
    pub full_autonomous: bool,
    pub autonomous_iteration_count: usize,
    pub autonomous_started_at: Option<String>,
    pub autonomous_last_progress_at: Option<String>,
    pub autonomous_pause_reason: Option<String>,
    pub autonomous_stop_reason: Option<String>,
    pub hidden_best_branch_id: Option<String>,
    pub active_retrieval_summary: Option<String>,
    pub strategy_summary: Option<String>,
    pub goal_summary: Option<String>,
    pub latest_diagnostics: Option<String>,
    pub active_node_id: Option<String>,
    pub active_branch_id: Option<String>,
    pub active_agent_role: Option<AgentRole>,
    pub active_foreground_branch_id: Option<String>,
    pub resolved_by_branch_id: Option<String>,
    pub hidden_branch_count: usize,
    pub imports: Vec<String>,
    pub nodes: Vec<ProofNode>,
    /// Live proof goals from Pantograph proof tree (shown in dashboard).
    pub proof_goals: Vec<ProofGoal>,
    /// Recent activity log for dashboard feed (last 50 entries).
    pub activity_log: Vec<ActivityEntry>,
    pub branches: Vec<ProofBranch>,
    pub agents: Vec<AgentRecord>,
    pub last_rendered_scratch: Option<String>,
    pub last_verification: Option<LeanVerificationSummary>,
    /// Accumulated LaTeX paper body, written incrementally by the model.
    pub paper_tex: String,
    /// Path to the persistent Scratch.lean file for this session.
    pub scratch_path: Option<String>,
    /// Path to the persistent Paper.tex file for this session.
    pub paper_path: Option<String>,
    /// How many lean verification attempts have been made.
    pub attempt_number: usize,
    /// Files in the session workspace (multi-file Lean project).
    pub workspace_files: Vec<WorkspaceFileEntry>,
    /// How many tool loop iterations were used in the last turn.
    pub tool_iteration_count: usize,
    /// Strategy for proof search: agentic, tactic search, or hybrid.
    pub search_strategy: SearchStrategy,
}

/// Strategy for autonomous proof search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SearchStrategy {
    /// Agents write and patch whole files (current behavior).
    Agentic,
    /// Pure tactic search at each sorry (no agentic loop).
    TacticSearch,
    /// Both run in parallel. First to solve a sorry wins.
    #[default]
    Hybrid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ShareMode {
    #[default]
    Local,
    Community,
    Private,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CloudPolicy {
    pub sync_enabled: bool,
    pub share_mode: ShareMode,
    pub private_overlay_community: bool,
    pub last_sync_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SessionSnapshot {
    pub id: String,
    pub title: String,
    pub updated_at: String,
    pub workspace_root: Option<String>,
    pub workspace_label: Option<String>,
    pub cloud: CloudPolicy,
    pub transcript: Vec<TranscriptEntry>,
    pub proof: ProofSessionState,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CorpusSummary {
    pub local_entry_count: usize,
    pub verified_entry_count: usize,
    pub cluster_count: usize,
    pub duplicate_member_count: usize,
    pub attempt_log_count: usize,
    pub library_seed_count: usize,
    pub user_verified_count: usize,
    pub latest_updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SyncSummary {
    pub pending_count: usize,
    pub failed_count: usize,
    pub sent_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LegacyImportSummary {
    pub imported: usize,
    pub skipped: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AuthSummary {
    pub logged_in: bool,
    pub auth_mode: Option<String>,
    pub email: Option<String>,
    pub plan: Option<String>,
    pub account_id: Option<String>,
    pub last_refresh: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LeanHealth {
    pub ok: bool,
    pub project_dir: Option<String>,
    pub lean_version: Option<String>,
    pub lake_version: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchRequest {
    pub query: String,
    pub limit: usize,
    pub share_mode: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub identity_key: String,
    pub label: String,
    pub statement: String,
    pub score: f32,
    pub visibility: String,
    pub artifact_id: Option<String>,
    pub decl_name: Option<String>,
    pub module_name: Option<String>,
    pub package_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponse {
    pub hits: Vec<SearchHit>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedUploadItem {
    pub identity_key: String,
    pub label: String,
    pub statement: String,
    pub artifact_id: Option<String>,
    pub artifact_content: String,
    pub visibility: String,
    pub decl_name: Option<String>,
    pub module_name: Option<String>,
    pub package_name: Option<String>,
    pub package_revision: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UploadVerifiedBatchRequest {
    pub visibility_scope: String,
    pub items: Vec<VerifiedUploadItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UploadVerifiedBatchResponse {
    pub accepted: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactResponse {
    pub artifact_id: String,
    pub identity_key: String,
    pub label: String,
    pub statement: String,
    pub artifact_content: String,
    pub visibility: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PackageSummary {
    pub package_name: String,
    pub package_revision: Option<String>,
    pub declaration_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PackageListResponse {
    pub packages: Vec<PackageSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CloudHealthResponse {
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DashboardSessionSummary {
    pub id: String,
    pub title: String,
    pub updated_at: String,
    pub workspace_label: Option<String>,
    pub transcript_entries: usize,
    pub proof_nodes: usize,
    pub active_node_label: Option<String>,
    pub proof_phase: Option<String>,
    pub last_role: Option<String>,
    pub last_excerpt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DashboardStatusResponse {
    pub local_db_path: String,
    pub auth: AuthSummary,
    pub lean: LeanHealth,
    pub session_count: usize,
    pub active_session_id: Option<String>,
    pub sessions: Vec<DashboardSessionSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct HealthReport {
    pub ok: bool,
    pub local_db_path: String,
    pub session_count: usize,
    pub latest_session_id: Option<String>,
    pub auth: AuthSummary,
    pub lean: LeanHealth,
}

// --- Cloud corpus types ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum VerifiedCorpusDeclKind {
    #[default]
    Theorem,
    Def,
    Opaque,
    Axiom,
    Inductive,
    Ctor,
    Recursor,
    Abbrev,
    Instance,
    Class,
    Structure,
    Unknown,
}

impl VerifiedCorpusDeclKind {
    pub fn from_str_normalized(s: &str) -> Self {
        match s.trim() {
            "theorem" => Self::Theorem,
            "def" => Self::Def,
            "opaque" => Self::Opaque,
            "axiom" => Self::Axiom,
            "inductive" => Self::Inductive,
            "ctor" => Self::Ctor,
            "recursor" => Self::Recursor,
            "abbrev" => Self::Abbrev,
            "instance" => Self::Instance,
            "class" => Self::Class,
            "structure" => Self::Structure,
            _ => Self::Unknown,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Theorem => "theorem",
            Self::Def => "def",
            Self::Opaque => "opaque",
            Self::Axiom => "axiom",
            Self::Inductive => "inductive",
            Self::Ctor => "ctor",
            Self::Recursor => "recursor",
            Self::Abbrev => "abbrev",
            Self::Instance => "instance",
            Self::Class => "class",
            Self::Structure => "structure",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct CloudCorpusUploadItem {
    pub identity_key: String,
    pub label: String,
    pub statement: String,
    pub artifact_content: String,
    pub artifact_id: Option<String>,
    pub verification_run_id: Option<String>,
    pub decl_name: Option<String>,
    pub module_name: Option<String>,
    pub package_name: Option<String>,
    pub package_revision: Option<String>,
    pub decl_kind: Option<String>,
    pub doc_string: Option<String>,
    pub namespace: Option<String>,
    pub imports: Vec<String>,
    pub environment_fingerprint: Option<String>,
    pub is_theorem_like: bool,
    pub is_instance: bool,
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct CloudCorpusSearchHit {
    pub id: String,
    pub statement_hash: String,
    pub identity_key: String,
    pub cluster_id: Option<String>,
    pub cluster_role: Option<String>,
    pub equivalence_confidence: Option<f64>,
    pub kind: String,
    pub label: String,
    pub statement: String,
    pub content_hash: String,
    pub artifact_id: String,
    pub verification_run_id: String,
    pub visibility: String,
    pub decl_name: Option<String>,
    pub module_name: Option<String>,
    pub package_name: Option<String>,
    pub package_revision: Option<String>,
    pub decl_kind: String,
    pub doc_string: Option<String>,
    pub search_text: String,
    pub origin: String,
    pub environment_fingerprint: Option<String>,
    pub is_theorem_like: bool,
    pub is_instance: bool,
    pub is_library_seed: bool,
    pub namespace: Option<String>,
    pub imports: Vec<String>,
    pub metadata: Value,
    pub source_session_id: Option<String>,
    pub source_node_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub artifact_content: String,
    pub score: f64,
    /// Goal types extracted from Lean (conclusion types of theorems/lemmas).
    pub goal_types: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct CloudCorpusSearchResponse {
    pub hits: Vec<CloudCorpusSearchHit>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct CloudCorpusUploadBatchRequest {
    pub visibility_scope: String,
    pub items: Vec<CloudCorpusUploadItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct CloudCorpusUploadBatchResponse {
    pub batch_id: String,
    pub promoted: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct CloudCorpusArtifactResponse {
    pub identity_key: String,
    pub artifact_content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct CloudCorpusPackageSummary {
    pub package_name: String,
    pub package_revision: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct CloudCorpusPackagesResponse {
    pub packages: Vec<CloudCorpusPackageSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct CloudCorpusSeedPackage {
    pub package_name: String,
    pub package_revision: Option<String>,
    pub source_type: Option<String>,
    pub source_url: Option<String>,
    pub manifest: Value,
    pub root_modules: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct CloudCorpusSeedModule {
    pub module_name: String,
    pub package_name: String,
    pub package_revision: Option<String>,
    pub source_path: Option<String>,
    pub imports: Vec<String>,
    pub environment_fingerprint: Option<String>,
    pub declaration_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct CloudCorpusSeedBatchRequest {
    pub packages: Vec<CloudCorpusSeedPackage>,
    pub modules: Vec<CloudCorpusSeedModule>,
    pub items: Vec<CloudCorpusUploadItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct CloudCorpusSeedBatchResponse {
    pub packages: usize,
    pub modules: usize,
    pub items: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct CloudCorpusAuthContext {
    pub auth_mode: Option<String>,
    pub bearer_token: Option<String>,
    pub account_id: Option<String>,
    pub dev_tenant_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct IngestionRunRecord {
    pub id: String,
    pub kind: String,
    pub environment_fingerprint: String,
    pub package_revision_set_hash: String,
    pub status: String,
    pub stats: Value,
    pub error: Option<String>,
    pub started_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct CorpusPackageRecord {
    pub id: String,
    pub package_name: String,
    pub package_revision: Option<String>,
    pub source_type: String,
    pub source_url: Option<String>,
    pub manifest: Value,
    pub root_modules: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct CorpusModuleRecord {
    pub id: String,
    pub module_name: String,
    pub package_name: String,
    pub package_revision: Option<String>,
    pub source_path: Option<String>,
    pub imports: Vec<String>,
    pub environment_fingerprint: String,
    pub declaration_count: usize,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct SyncQueueItem {
    pub id: String,
    pub session_id: Option<String>,
    pub queue_type: String,
    pub payload_json: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct IngestLibrarySeedResult {
    pub run_id: Option<String>,
    pub skipped: bool,
    pub package_count: usize,
    pub module_count: usize,
    pub declaration_count: usize,
    pub environment_fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct RemoteCorpusAvailability {
    pub enabled_by_flag: bool,
    pub base_url: Option<String>,
    pub available: bool,
    pub reason: String,
}
