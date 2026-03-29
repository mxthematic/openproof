import React, { useEffect, useState, useCallback } from "https://esm.sh/react@18.3.1";
import { createRoot } from "https://esm.sh/react-dom@18.3.1/client";
import htm from "https://esm.sh/htm@3.1.1";
import { GraphTab } from "/graph.js";
import { SessionSidebar } from "/sessions.js";

const h = htm.bind(React.createElement);
const POLL_MS = 2000;
const STATUS_POLL_MS = 10000;

// ── Helpers ─────────────────────────────────────────────────────────────

function statusDot(status) {
  const s = String(status || "").toLowerCase();
  if (s === "verified") return "dot-verified";
  if (s === "proving") return "dot-proving";
  if (s === "failed") return "dot-failed";
  return "dot-pending";
}

function badgeClass(ok) {
  if (ok === true) return "badge badge-green";
  if (ok === false) return "badge badge-red";
  return "badge badge-yellow";
}

// ── Decomposition scoring (client-side, mirrors Rust logic) ────────────

function scoreLeaf(branch) {
  const history = branch?.search_history || [];
  if (history.length === 0) return 0.5;
  if (history.some((h) => h.outcome === "solved")) return 1.0;
  if (history.length < 2) return 0.4;

  let score = 0.5;
  const goals = history.map((h) => h.remaining_goals);
  const improving = goals.some((g, i) => i > 0 && g < goals[i - 1]);
  const flatlined = goals.every((g, i) => i === 0 || g === goals[i - 1]);

  if (improving) score += 0.2;
  if (flatlined && history.length >= 3) score -= 0.3;
  if (history.filter((h) => h.timed_out).length >= 2) score -= 0.3;
  if (history.filter((h) => h.outcome === "exhausted" && h.remaining_goals > 0).length >= 2) score -= 0.2;

  return Math.max(0, Math.min(1, score));
}

function computeSubtreeScores(nodes, branches) {
  const scores = {};
  // Leaf scores from branches
  const leafScores = {};
  for (const b of branches) {
    const nid = b.focus_node_id || b.focusNodeId;
    if (nid) {
      const s = scoreLeaf(b);
      leafScores[nid] = Math.max(leafScores[nid] || 0, s);
    }
  }
  // Verified nodes = solved
  for (const n of nodes) {
    if ((n.status || "").toLowerCase() === "verified") leafScores[n.id] = 1.0;
  }
  // Parent -> children map
  const childrenOf = {};
  for (const n of nodes) {
    const pid = n.parent_id || n.parentId;
    if (pid) {
      if (!childrenOf[pid]) childrenOf[pid] = [];
      childrenOf[pid].push(n.id);
    }
  }
  // Bottom-up: sort by depth descending
  const sorted = [...nodes].sort((a, b) => (b.depth || 0) - (a.depth || 0));
  for (const node of sorted) {
    const kids = childrenOf[node.id];
    if (!kids || kids.length === 0) {
      const ls = leafScores[node.id] ?? 0.5;
      scores[node.id] = { score: ls, worstChild: null, solved: ls >= 1.0 ? 1 : 0, total: 1 };
    } else {
      let minScore = 1.0, worstId = null, solved = 0;
      for (const cid of kids) {
        const cs = scores[cid]?.score ?? 0.5;
        if (cs >= 1.0) solved++;
        if (cs < minScore) { minScore = cs; worstId = cid; }
      }
      scores[node.id] = { score: minScore, worstChild: worstId, solved, total: kids.length };
    }
  }
  return scores;
}

function scoreColor(score) {
  if (score >= 0.8) return "#4ade80";  // green
  if (score >= 0.5) return "#facc15";  // yellow
  if (score >= 0.3) return "#fb923c";  // orange
  return "#ef4444";                     // red
}

function formatSearchHistory(history) {
  if (!history || history.length === 0) return null;
  const last = history[history.length - 1];
  const goals = history.map((h) => h.remaining_goals);
  const trend = goals.length >= 2
    ? (goals[goals.length - 1] < goals[goals.length - 2] ? "improving" : goals[goals.length - 1] === goals[goals.length - 2] ? "flat" : "regressing")
    : "n/a";
  return `${history.length} searches | last: ${last.outcome} (${last.remaining_goals} goals, ${last.expansions} exp) | trend: ${trend}`;
}

// ── App ─────────────────────────────────────────────────────────────────

function App() {
  const [sessions, setSessions] = useState([]);
  const [selectedId, setSelectedId] = useState(null);
  const [session, setSession] = useState(null);
  const [tab, setTab] = useState("overview");
  const [status, setStatus] = useState(null);
  const [refreshKey, setRefreshKey] = useState(0);

  const triggerRefresh = useCallback(() => setRefreshKey((k) => k + 1), []);

  // Poll lightweight session summaries for the sidebar
  useEffect(() => {
    let c = false;
    async function poll() {
      try {
        const r = await fetch("/api/session-summaries");
        const d = await r.json();
        if (c) return;
        setSessions(d || []);
        setSelectedId((cur) => cur || d?.[0]?.id || null);
      } catch {}
    }
    poll();
    const t = setInterval(poll, POLL_MS);
    return () => { c = true; clearInterval(t); };
  }, [refreshKey]);

  // Poll status (lean health, auth) at a lower frequency
  useEffect(() => {
    let c = false;
    async function poll() {
      try {
        const r = await fetch("/api/status");
        const d = await r.json();
        if (!c) setStatus(d);
      } catch {}
    }
    poll();
    const t = setInterval(poll, STATUS_POLL_MS);
    return () => { c = true; clearInterval(t); };
  }, []);

  // Poll selected session detail
  useEffect(() => {
    let c = false;
    if (!selectedId) { setSession(null); return () => { c = true; }; }
    async function poll() {
      try {
        const r = await fetch(`/api/session?id=${encodeURIComponent(selectedId)}`);
        if (r.status === 404) { if (!c) setSession(null); return; }
        const d = await r.json();
        if (!c) setSession(d);
      } catch {}
    }
    poll();
    const t = setInterval(poll, POLL_MS);
    return () => { c = true; clearInterval(t); };
  }, [selectedId]);

  const proof = session?.proof;
  const leanOk = status?.lean?.ok;

  return h`
    <div className="header">
      <span className="header-brand">openproof</span>
      <span className="header-sep" />
      <span className="header-item"><strong>${session?.title || "no session"}</strong></span>
      <span className="header-sep" />
      <span className="header-item">${proof?.phase || "idle"}</span>
      <span className="header-sep" />
      <span className=${badgeClass(leanOk)}>Lean ${leanOk ? "ok" : "?"}</span>
      ${proof?.last_verification ? h`
        <span className=${badgeClass(proof.last_verification.ok)}>
          ${proof.last_verification.ok ? "verified" : "failed"}
        </span>
      ` : null}
    </div>

    <div className="layout">
      <${SessionSidebar}
        sessions=${sessions}
        selectedId=${selectedId}
        onSelect=${setSelectedId}
        onChanged=${triggerRefresh} />

      <div className="main-area">
        <div className="tabs">
          ${["overview", "graph", "code", "activity", "paper"].map((t) => h`
            <button key=${t}
              className=${`tab ${tab === t ? "tab-active" : ""}`}
              onClick=${() => setTab(t)}>
              ${t.charAt(0).toUpperCase() + t.slice(1)}
            </button>
          `)}
        </div>
        <div className="tab-content">
          ${!session ? h`<div className="empty">Select a session</div>`
            : tab === "overview" ? h`<${OverviewTab} session=${session} />`
            : tab === "graph" ? h`<${GraphTab} session=${session} />`
            : tab === "code" ? h`<${CodeTab} sessionId=${selectedId} />`
            : tab === "activity" ? h`<${ActivityTab} session=${session} />`
            : h`<${PaperTab} sessionId=${selectedId} />`}
        </div>
      </div>
    </div>
  `;
}

// ── Overview Tab ─────────────────────────────────────────────────────────

function OverviewTab({ session }) {
  const proof = session?.proof;
  const nodes = proof?.nodes || [];
  const branches = proof?.branches || [];
  const verification = proof?.last_verification;
  const subtreeScores = computeSubtreeScores(nodes, branches);

  // Build branch lookup by focus node
  const branchByNode = {};
  for (const b of branches) {
    const nid = b.focus_node_id || b.focusNodeId;
    if (nid) branchByNode[nid] = b;
  }

  return h`
    <div className="overview">
      <div className="overview-panel">
        <div className="panel-title">Proof Tree (${nodes.length} nodes)</div>
        <div className="panel-body">
          ${nodes.length === 0 ? h`<div style=${{ padding: "12px", color: "var(--muted)" }}>No nodes yet</div>` : null}
          ${nodes.map((n) => {
            const ss = subtreeScores[n.id];
            const sc = ss?.score ?? null;
            const branch = branchByNode[n.id];
            const searchInfo = branch ? formatSearchHistory(branch.search_history) : null;
            const indent = (n.depth || 0) * 16;
            return h`
              <div key=${n.id} className="node-row" style=${{ paddingLeft: indent + "px" }}>
                <div className=${`node-dot ${statusDot(n.status)}`} />
                ${sc !== null ? h`
                  <span className="node-score" style=${{ color: scoreColor(sc) }}>
                    ${(sc * 100).toFixed(0)}%
                  </span>
                ` : null}
                <span className="node-kind">${n.kind || "node"}</span>
                <span className="node-label">${n.label}</span>
                <span className="node-statement">${n.statement}</span>
                ${ss && ss.total > 1 ? h`
                  <span className="node-children-info">
                    (${ss.solved}/${ss.total} solved)
                  </span>
                ` : null}
                ${searchInfo ? h`
                  <div className="node-search-info">${searchInfo}</div>
                ` : null}
              </div>
            `;
          })}
          ${verification ? h`
            <div className="verify-banner">
              <span className=${verification.ok ? "verify-pass" : "verify-fail"}>
                ${verification.ok ? "Lean verified" : "Lean failed"}
              </span>
              ${!verification.ok && verification.stderr ? h`
                <div className="verify-detail">${verification.stderr}</div>
              ` : null}
            </div>
          ` : null}
        </div>
      </div>

      <div className="overview-panel">
        <div className="panel-title">Branches (${branches.length})</div>
        <div className="panel-body">
          ${branches.length === 0 ? h`<div style=${{ padding: "12px", color: "var(--muted)" }}>No branches yet</div>` : null}
          ${branches.map((b) => {
            const history = b.search_history || [];
            const lastSearch = history.length > 0 ? history[history.length - 1] : null;
            return h`
              <div key=${b.id} className="branch-card">
                <div className="branch-header">
                  <span className="branch-role">${b.role}</span>
                  <span className="branch-title">${b.title}</span>
                  <span className=${badgeClass(b.status === "done")}>${b.status}</span>
                  ${b.attempt_count > 0 ? h`
                    <span className="branch-attempts">${b.attempt_count} attempts</span>
                  ` : null}
                </div>
                ${lastSearch ? h`
                  <div className="branch-search-metrics">
                    Last search: ${lastSearch.outcome}
                    ${lastSearch.remaining_goals != null ? ` | ${lastSearch.remaining_goals} goals remaining` : ""}
                    ${lastSearch.expansions ? ` | ${lastSearch.expansions} expansions` : ""}
                    ${lastSearch.timed_out ? " | TIMED OUT" : ""}
                  </div>
                ` : null}
                ${history.length > 1 ? h`
                  <div className="branch-search-trend">
                    Goals trend: [${history.map((h) => h.remaining_goals).join(", ")}]
                  </div>
                ` : null}
                ${b.lean_snippet || b.leanSnippet ? h`
                  <pre className="branch-snippet">${b.lean_snippet || b.leanSnippet}</pre>
                ` : null}
                ${b.summary ? h`<div className="branch-status">${b.summary}</div>` : null}
              </div>
            `;
          })}
        </div>
      </div>
    </div>
  `;
}

// ── Activity Tab ────────────────────────────────────────────────────────

function ActivityTab({ session }) {
  const proof = session?.proof;
  const activityLog = proof?.activity_log || proof?.activityLog || [];
  const goals = proof?.proof_goals || proof?.proofGoals || [];

  return h`
    <div style=${{ padding: 16, fontFamily: "monospace", fontSize: 12, maxHeight: "calc(100vh - 180px)", overflow: "auto" }}>
      <h3 style=${{ color: "#e5e5e5", margin: "0 0 12px", fontSize: 14 }}>Live Activity</h3>
      ${activityLog.length === 0 ? h`
        <div style=${{ color: "#525252" }}>No activity yet. Start a proof to see events here.</div>
      ` : h`
        <div style=${{ display: "flex", flexDirection: "column", gap: 4 }}>
          ${activityLog.slice().reverse().map((entry, i) => {
            const kindColors = { tool: "#06b6d4", verify: "#22c55e", search: "#f59e0b", error: "#ef4444" };
            const color = kindColors[entry.kind] || "#737373";
            const time = entry.timestamp ? new Date(entry.timestamp).toLocaleTimeString() : "";
            return h`
              <div key=${i} style=${{ display: "flex", gap: 8, alignItems: "baseline" }}>
                <span style=${{ color: "#525252", fontSize: 10, minWidth: 70 }}>${time}</span>
                <span style=${{ color, fontSize: 10, fontWeight: 600, minWidth: 40 }}>${entry.kind}</span>
                <span style=${{ color: "#a3a3a3" }}>${entry.message}</span>
              </div>
            `;
          })}
        </div>
      `}
      ${goals.length > 0 ? h`
        <h3 style=${{ color: "#e5e5e5", margin: "16px 0 8px", fontSize: 14 }}>Proof Goals (${goals.length})</h3>
        ${goals.map((g, i) => {
          const statusColors = { open: "#f59e0b", in_progress: "#3b82f6", closed: "#22c55e", failed: "#ef4444" };
          const color = statusColors[g.status] || "#737373";
          const failCount = (g.failed_tactics || g.failedTactics || []).length;
          return h`
            <div key=${i} style=${{ padding: "6px 8px", margin: "4px 0", border: "1px solid #333", borderRadius: 4, borderLeftColor: color, borderLeftWidth: 3 }}>
              <div style=${{ display: "flex", gap: 8, alignItems: "center" }}>
                <span style=${{ color, fontWeight: 600, fontSize: 11 }}>${g.status}</span>
                ${failCount > 0 ? h`<span style=${{ color: "#ef4444", fontSize: 10 }}>${failCount} failed tactics</span>` : null}
                ${g.attempts > 0 ? h`<span style=${{ color: "#525252", fontSize: 10 }}>${g.attempts} attempts</span>` : null}
              </div>
              <div style=${{ color: "#a3a3a3", fontSize: 11, marginTop: 2 }}>
                ${(g.goal_text || g.goalText || "").substring(0, 120)}
              </div>
              ${g.tactic_applied || g.tacticApplied ? h`
                <div style=${{ color: "#22c55e", fontSize: 10, marginTop: 2 }}>via: ${g.tactic_applied || g.tacticApplied}</div>
              ` : null}
            </div>
          `;
        })}
      ` : null}
    </div>
  `;
}

// ── Lean Syntax Highlighting ────────────────────────────────────────────

const LEAN_KEYWORDS = new Set([
  "theorem", "lemma", "def", "abbrev", "instance", "class", "structure",
  "where", "by", "have", "let", "show", "suffices", "calc", "match", "with",
  "if", "then", "else", "do", "return", "for", "in", "open", "import",
  "namespace", "end", "section", "variable", "example", "noncomputable",
  "sorry", "exact", "apply", "intro", "intros", "rw", "simp", "omega",
  "ring", "norm_num", "linarith", "nlinarith", "aesop", "trivial",
  "constructor", "rcases", "obtain", "refine", "cases", "induction",
  "contradiction", "exfalso", "push_neg", "classical", "decide",
]);
const LEAN_TYPES = new Set([
  "Prop", "Type", "Sort", "Nat", "Int", "Bool", "String", "List", "Option",
  "True", "False", "And", "Or", "Not", "Iff", "Exists", "Finset", "Set",
]);

function highlightLean(line) {
  if (line.trimStart().startsWith("--") || line.trimStart().startsWith("/-")) {
    return h`<span style=${{ color: "#525252", fontStyle: "italic" }}>${line}</span>`;
  }
  if (line.includes("sorry")) {
    const parts = line.split("sorry");
    const result = [];
    for (let i = 0; i < parts.length; i++) {
      if (i > 0) result.push(h`<span style=${{ color: "#ef4444", fontWeight: 600 }}>sorry</span>`);
      result.push(highlightTokens(parts[i]));
    }
    return h`<span>${result}</span>`;
  }
  return highlightTokens(line);
}

function highlightTokens(text) {
  return text.replace(/\b(\w+)\b/g, (match) => {
    if (LEAN_KEYWORDS.has(match)) return `\x01kw\x02${match}\x01/kw\x02`;
    if (LEAN_TYPES.has(match)) return `\x01ty\x02${match}\x01/ty\x02`;
    return match;
  }).split(/(\x01kw\x02[^\x01]*\x01\/kw\x02|\x01ty\x02[^\x01]*\x01\/ty\x02)/).map((part, i) => {
    if (part.startsWith("\x01kw\x02")) return h`<span key=${i} style=${{ color: "#c084fc" }}>${part.slice(4, -5)}</span>`;
    if (part.startsWith("\x01ty\x02")) return h`<span key=${i} style=${{ color: "#22d3ee" }}>${part.slice(4, -5)}</span>`;
    return part;
  });
}

// ── Code Tab ────────────────────────────────────────────────────────────

function CodeTab({ sessionId }) {
  const filesRef = React.useRef([]);
  const [filePaths, setFilePaths] = useState([]);
  const [selected, setSelected] = useState(null);
  const [content, setContent] = useState("");

  useEffect(() => {
    if (!sessionId) return;
    let c = false;
    async function poll() {
      try {
        const r = await fetch(`/api/workspace?id=${encodeURIComponent(sessionId)}`);
        const d = await r.json();
        if (c) return;
        const f = d.files || d || [];
        filesRef.current = f;
        const newPaths = f.map(x => x.path).join(",");
        const oldPaths = filePaths.join(",");
        if (newPaths !== oldPaths) {
          setFilePaths(f.map(x => x.path));
        }
        const sel = selected || (f.length > 0 ? f[0].path : null);
        if (!selected && f.length > 0) setSelected(f[0].path);
        const cur = f.find(x => x.path === sel);
        if (cur) setContent(cur.content || "");
      } catch {}
    }
    poll();
    const t = setInterval(poll, 4000);
    return () => { c = true; clearInterval(t); };
  }, [sessionId, selected]);

  const selectFile = useCallback((path) => {
    setSelected(path);
    const cur = filesRef.current.find(f => f.path === path);
    if (cur) setContent(cur.content || "");
  }, []);

  const lines = content.split("\n");

  return h`
    <div style=${{ display: "flex", height: "100%", gap: 0 }}>
      <div style=${{ width: 180, borderRight: "1px solid #262626", padding: "8px 0", overflow: "auto", flexShrink: 0 }}>
        ${filePaths.map(path => h`
          <button key=${path}
            onClick=${() => selectFile(path)}
            style=${{
              display: "block", width: "100%", textAlign: "left",
              padding: "6px 12px", border: "none", cursor: "pointer",
              background: selected === path ? "#1e293b" : "transparent",
              color: selected === path ? "#e5e5e5" : "#737373",
              fontSize: 12, fontFamily: "'SF Mono', 'Fira Code', monospace",
              borderLeft: selected === path ? "2px solid #3b82f6" : "2px solid transparent",
            }}>
            ${path}
          </button>
        `)}
      </div>
      <div style=${{ flex: 1, overflow: "auto", padding: 0 }}>
        ${content ? h`
          <pre style=${{
            margin: 0, padding: "12px 0", fontFamily: "'SF Mono', 'Fira Code', Consolas, monospace",
            fontSize: 13, lineHeight: 1.6, color: "#e5e5e5", background: "#0a0a0a",
            tabSize: 2, minHeight: "100%",
          }}>
            ${lines.map((line, i) => h`
              <div key=${i} style=${{ display: "flex", minHeight: "1.6em",
                background: line.includes("sorry") ? "rgba(239,68,68,0.08)" : "transparent",
              }}>
                <span style=${{
                  color: "#404040", width: 48, textAlign: "right", paddingRight: 16,
                  userSelect: "none", flexShrink: 0, borderRight: "1px solid #1a1a1a",
                  marginRight: 16,
                }}>${i + 1}</span>
                <code>${highlightLean(line)}</code>
              </div>
            `)}
          </pre>
        ` : h`<div style=${{ color: "#737373", padding: 20 }}>Select a file</div>`}
      </div>
    </div>
  `;
}

// ── Paper Tab ───────────────────────────────────────────────────────────

function PaperTab({ sessionId }) {
  const [view, setView] = useState("pdf");
  const [tex, setTex] = useState("");
  const [pdfUrl, setPdfUrl] = useState(null);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(true);

  const loadPdf = useCallback(async () => {
    if (!sessionId) return;
    setLoading(true);
    setError("");
    try {
      const r = await fetch(`/api/paper/pdf?id=${encodeURIComponent(sessionId)}`);
      if (!r.ok) {
        const text = await r.text();
        setError(text);
        setPdfUrl(null);
      } else {
        const blob = await r.blob();
        setPdfUrl(URL.createObjectURL(blob));
      }
    } catch (e) {
      setError(String(e));
    }
    setLoading(false);
  }, [sessionId]);

  const loadTex = useCallback(async () => {
    if (!sessionId) return;
    try {
      const r = await fetch(`/api/paper/tex?id=${encodeURIComponent(sessionId)}`);
      setTex(await r.text());
    } catch {}
  }, [sessionId]);

  useEffect(() => { loadPdf(); loadTex(); }, [loadPdf, loadTex]);

  return h`
    <div className="paper-container">
      <div className="paper-toolbar">
        <button className=${view === "pdf" ? "active" : ""} onClick=${() => setView("pdf")}>
          Compiled PDF
        </button>
        <button className=${view === "tex" ? "active" : ""} onClick=${() => setView("tex")}>
          TeX Source
        </button>
        <button onClick=${loadPdf} style=${{ marginLeft: "auto" }}>Recompile</button>
      </div>
      <div className="paper-body">
        ${view === "pdf" ? (
          loading ? h`<div className="paper-loading">Compiling...</div>`
          : error ? h`<div className="paper-error">${error}</div>`
          : pdfUrl ? h`<embed src=${pdfUrl} type="application/pdf" />`
          : h`<div className="paper-loading">No PDF available</div>`
        ) : h`<textarea className="paper-source" value=${tex} readOnly />`}
      </div>
    </div>
  `;
}

// ── Mount ───────────────────────────────────────────────────────────────

createRoot(document.getElementById("root")).render(h`<${App} />`);
