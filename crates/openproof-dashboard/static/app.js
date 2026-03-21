import React, { useEffect, useMemo, useState, useCallback } from "https://esm.sh/react@18.3.1";
import { createRoot } from "https://esm.sh/react-dom@18.3.1/client";
import htm from "https://esm.sh/htm@3.1.1";
import { ReactFlow, Background, Controls, MiniMap, Handle, Position, useNodesState, useEdgesState } from "https://esm.sh/@xyflow/react@12.6.0?deps=react@18.3.1,react-dom@18.3.1";

const h = htm.bind(React.createElement);
const POLL_MS = 2000;

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

// ── App ─────────────────────────────────────────────────────────────────

function App() {
  const [sessions, setSessions] = useState([]);
  const [selectedId, setSelectedId] = useState(null);
  const [session, setSession] = useState(null);
  const [tab, setTab] = useState("overview");
  const [status, setStatus] = useState(null);

  // Poll sessions list
  useEffect(() => {
    let c = false;
    async function poll() {
      try {
        const r = await fetch("/api/status");
        const d = await r.json();
        if (c) return;
        setStatus(d);
        setSessions(d.sessions || []);
        setSelectedId((cur) => cur || d.activeSessionId || d.sessions?.[0]?.id || null);
      } catch {}
    }
    poll();
    const t = setInterval(poll, POLL_MS);
    return () => { c = true; clearInterval(t); };
  }, []);

  // Poll selected session
  useEffect(() => {
    let c = false;
    if (!selectedId) { setSession(null); return () => { c = true; }; }
    async function poll() {
      try {
        const r = await fetch(`/api/session?id=${encodeURIComponent(selectedId)}`);
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
      <div className="sidebar">
        <div className="sidebar-title">Sessions</div>
        ${sessions.map((s) => h`
          <button key=${s.id}
            className=${`session-item ${selectedId === s.id ? "session-item-active" : ""}`}
            onClick=${() => setSelectedId(s.id)}>
            <strong>${s.title}</strong>
            <small>${s.transcriptEntries || 0} entries \u00b7 ${s.proofNodes || 0} nodes</small>
          </button>
        `)}
      </div>

      <div className="main-area">
        <div className="tabs">
          ${["overview", "graph", "paper"].map((t) => h`
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

  return h`
    <div className="overview">
      <div className="overview-panel">
        <div className="panel-title">Proof Nodes (${nodes.length})</div>
        <div className="panel-body">
          ${nodes.length === 0 ? h`<div style=${{ padding: "12px", color: "var(--muted)" }}>No nodes yet</div>` : null}
          ${nodes.map((n) => h`
            <div key=${n.id} className="node-row">
              <div className=${`node-dot ${statusDot(n.status)}`} />
              <span className="node-kind">${n.kind || "node"}</span>
              <span className="node-label">${n.label}</span>
              <span className="node-statement">${n.statement}</span>
            </div>
          `)}
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
          ${branches.map((b) => h`
            <div key=${b.id} className="branch-card">
              <div className="branch-header">
                <span className="branch-role">${b.role}</span>
                <span className="branch-title">${b.title}</span>
                <span className=${badgeClass(b.status === "done")}>${b.status}</span>
              </div>
              ${b.lean_snippet || b.leanSnippet ? h`
                <pre className="branch-snippet">${b.lean_snippet || b.leanSnippet}</pre>
              ` : null}
              ${b.summary ? h`<div className="branch-status">${b.summary}</div>` : null}
            </div>
          `)}
        </div>
      </div>
    </div>
  `;
}

// ── Graph Tab (React Flow) ──────────────────────────────────────────────

const statusColor = (s) => {
  const st = String(s || "").toLowerCase();
  if (st === "verified" || st === "done") return "#22c55e";
  if (st === "proving" || st === "running") return "#eab308";
  if (st === "failed" || st === "error" || st === "blocked") return "#ef4444";
  return "#525252";
};

const roleColor = (r) => {
  const role = String(r || "").toLowerCase();
  if (role === "prover") return "#3b82f6";
  if (role === "repairer") return "#f59e0b";
  if (role === "planner") return "#8b5cf6";
  if (role === "retriever") return "#06b6d4";
  if (role === "critic") return "#ec4899";
  return "#6b7280";
};

const kindIcon = (k) => {
  const kind = String(k || "").toLowerCase();
  if (kind === "theorem") return "\u{1D4AF}";
  if (kind === "lemma") return "\u{2113}";
  if (kind === "def" || kind === "artifact") return "\u{1D49F}";
  if (kind === "axiom") return "\u{1D49C}";
  return "\u25CB";
};

function ProofNodeComponent({ data }) {
  const borderColor = statusColor(data.status);
  return h`
    <div style=${{
      background: "#1a1a1a",
      border: "2px solid " + borderColor,
      borderRadius: 8,
      padding: "8px 12px",
      minWidth: 160,
      maxWidth: 240,
      fontFamily: "system-ui, sans-serif",
    }}>
      <${Handle} type="target" position=${Position.Top} style=${{ background: "#555" }} />
      <div style=${{ display: "flex", alignItems: "center", gap: 6, marginBottom: 4 }}>
        <span style=${{ fontSize: 14 }}>${kindIcon(data.kind)}</span>
        <strong style=${{ color: "#e5e5e5", fontSize: 12 }}>${data.label}</strong>
      </div>
      <div style=${{ color: "#a3a3a3", fontSize: 10, marginBottom: 2 }}>
        ${data.kind || "node"} \u00b7 ${data.status}
      </div>
      ${data.statement ? h`
        <div style=${{ color: "#525252", fontSize: 9, fontFamily: "monospace", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", maxWidth: 220 }}>
          ${data.statement}
        </div>
      ` : null}
      <${Handle} type="source" position=${Position.Bottom} style=${{ background: "#555" }} />
    </div>
  `;
}

function BranchNodeComponent({ data }) {
  const color = roleColor(data.role);
  const hasSnippet = !!(data.lean_snippet || data.leanSnippet || "").trim();
  return h`
    <div style=${{
      background: "#111",
      border: "1.5px solid " + color,
      borderRadius: 5,
      padding: "6px 10px",
      minWidth: 130,
      opacity: hasSnippet ? 1 : 0.7,
      fontFamily: "system-ui, sans-serif",
    }}>
      <${Handle} type="target" position=${Position.Top} style=${{ background: color }} />
      <div style=${{ color, fontSize: 10, fontWeight: 600 }}>
        ${data.role}${data.hidden ? " (hidden)" : ""}
        ${hasSnippet ? h`<span style=${{ color: "#22c55e", marginLeft: 4 }}>\u25CF</span>` : null}
      </div>
      <div style=${{ color: "#737373", fontSize: 9 }}>
        ${String(data.status || "idle")} \u00b7 score ${(data.score || 0).toFixed(0)} \u00b7 ${data.attempt_count || data.attemptCount || 0} tries
      </div>
      ${data.summary ? h`
        <div style=${{ color: "#525252", fontSize: 8, marginTop: 2, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", maxWidth: 180 }}>
          ${data.summary}
        </div>
      ` : null}
    </div>
  `;
}

const nodeTypes = {
  proofNode: ProofNodeComponent,
  branchNode: BranchNodeComponent,
};

function GraphTab({ session }) {
  const proof = session?.proof;
  const proofNodes = proof?.nodes || [];
  const allBranches = proof?.branches || [];
  const [showBranches, setShowBranches] = useState(false);
  const branches = showBranches ? allBranches : [];

  if (proofNodes.length === 0 && allBranches.length === 0) {
    return h`<div className="graph-container">No proof nodes to visualize</div>`;
  }

  // Build React Flow nodes and edges
  const { flowNodes, flowEdges } = useMemo(() => {
    const nodes = [];
    const edges = [];

    // Group proof nodes by depth for layout
    const byDepth = {};
    for (const n of proofNodes) {
      const d = n.depth || 0;
      if (!byDepth[d]) byDepth[d] = [];
      byDepth[d].push(n);
    }

    // Position proof nodes in a tree
    for (const n of proofNodes) {
      const d = n.depth || 0;
      const siblings = byDepth[d] || [];
      const idx = siblings.indexOf(n);
      const totalWidth = siblings.length * 220;
      const startX = -(totalWidth / 2) + 110;

      nodes.push({
        id: n.id,
        type: "proofNode",
        position: { x: startX + idx * 220, y: d * 100 },
        draggable: true,
        data: { ...n, _nodeColor: statusColor(n.status) },
      });

      // Parent edge
      const parentId = n.parent_id || n.parentId;
      if (parentId) {
        edges.push({
          id: "tree-" + n.id,
          source: parentId,
          target: n.id,
          style: { stroke: "#3b82f6", strokeWidth: 2 },
          animated: n.status === "proving",
        });
      }

      // Dependency edges
      for (const depId of (n.depends_on || n.dependsOn || [])) {
        edges.push({
          id: "dep-" + n.id + "-" + depId,
          source: depId,
          target: n.id,
          style: { stroke: "#6b7280", strokeWidth: 1, strokeDasharray: "4 3" },
        });
      }
    }

    // Position branches below the tree
    const maxDepth = Math.max(0, ...proofNodes.map((n) => n.depth || 0));
    const branchY = (maxDepth + 1) * 100 + 40;
    const branchCols = {};

    for (const b of branches) {
      const focusId = b.focus_node_id || b.focusNodeId || proofNodes[0]?.id;
      if (!branchCols[focusId]) branchCols[focusId] = 0;
      const col = branchCols[focusId]++;

      const parentNode = nodes.find((n) => n.id === focusId);
      const baseX = parentNode ? parentNode.position.x : col * 160;

      const bId = "branch-" + b.id;
      nodes.push({
        id: bId,
        type: "branchNode",
        position: { x: baseX + col * 160, y: branchY },
        draggable: true,
        data: { ...b, _nodeColor: roleColor(b.role) },
      });

      if (focusId) {
        edges.push({
          id: "agent-" + b.id,
          source: focusId,
          target: bId,
          style: { stroke: roleColor(b.role), strokeWidth: 1, strokeDasharray: "4 3", opacity: 0.5 },
        });
      }
    }

    return { flowNodes: nodes, flowEdges: edges };
  }, [proofNodes, branches]);

  const [rfNodes, setRfNodes, onNodesChange] = useNodesState(flowNodes);
  const [rfEdges, setRfEdges, onEdgesChange] = useEdgesState(flowEdges);

  // Sync data from polling without resetting user-dragged positions
  useEffect(() => {
    setRfNodes((prev) => {
      const prevById = {};
      for (const n of prev) prevById[n.id] = n;
      // Update existing nodes' data but keep their position if they were dragged
      const merged = flowNodes.map((fn) => {
        const existing = prevById[fn.id];
        if (existing) {
          return { ...fn, position: existing.position };
        }
        return fn;
      });
      return merged;
    });
    setRfEdges(flowEdges);
  }, [flowNodes, flowEdges]);

  const verification = proof?.last_verification;
  const attemptNum = proof?.attempt_number || proof?.attemptNumber || 0;

  return h`
    <div className="graph-canvas" style=${{ height: "100%", minHeight: 400 }}>
      <div className="graph-info">
        <span>Phase: <strong>${proof?.phase || "idle"}</strong></span>
        <span>\u00a0\u00b7\u00a0 Proof nodes: ${proofNodes.length}</span>
        <span>\u00a0\u00b7\u00a0 Attempts: ${attemptNum}</span>
        <button onClick=${() => setShowBranches(!showBranches)} style=${{
          marginLeft: 12, padding: "2px 8px", fontSize: 10, cursor: "pointer",
          background: showBranches ? "#333" : "#1a1a1a", color: "#a3a3a3",
          border: "1px solid #333", borderRadius: 4,
        }}>${showBranches ? "Hide" : "Show"} agent branches (${allBranches.length})</button>
        ${verification ? h`
          <span>\u00a0\u00b7\u00a0
            <span style=${{ color: verification.ok ? "#22c55e" : "#ef4444" }}>
              ${verification.ok ? "Lean verified" : "Lean failed"}
            </span>
          </span>
        ` : null}
      </div>
      <div style=${{ width: "100%", height: "calc(100% - 40px)" }}>
        <${ReactFlow}
          nodes=${rfNodes}
          edges=${rfEdges}
          onNodesChange=${onNodesChange}
          onEdgesChange=${onEdgesChange}
          nodeTypes=${nodeTypes}
          fitView
          fitViewOptions=${{ padding: 0.3 }}
          minZoom=${0.2}
          maxZoom=${2}
          defaultViewport=${{ x: 0, y: 0, zoom: 0.8 }}
          proOptions=${{ hideAttribution: true }}
          style=${{ background: "#0a0a0a" }}
        >
          <${Background} color="#222" gap=${20} />
          <${Controls} position="bottom-right" />
          <${MiniMap}
            nodeColor=${(n) => n.data?._nodeColor || "#525252"}
            maskColor="rgba(0,0,0,0.7)"
            style=${{ background: "#111" }}
          />
        <//>
      </div>
    </div>
  `;
}

// ── Paper Tab ───────────────────────────────────────────────────────────

function PaperTab({ sessionId }) {
  const [view, setView] = useState("pdf"); // "pdf" or "tex"
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
