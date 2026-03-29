import { useEffect, useRef, useState } from "react";
import { LeanMonaco, LeanMonacoEditor } from "lean4monaco";
import "./editor-styles.css";

const DEFAULT_CONTENT = "import Mathlib\n\n-- Write your Lean 4 code here\n";

export function EditorApp() {
  const wrapperRef = useRef<HTMLDivElement>(null);
  const editorRef = useRef<HTMLDivElement>(null);
  const infoviewRef = useRef<HTMLDivElement>(null);
  const leanMonacoRef = useRef<LeanMonaco | null>(null);
  const leanEditorRef = useRef<LeanMonacoEditor | null>(null);
  const [status, setStatus] = useState<"loading" | "ready" | "error">(
    "loading"
  );
  const [files, setFiles] = useState<{ path: string; content: string }[]>([]);
  const [selectedFile, setSelectedFile] = useState("Scratch.lean");

  const sessionId = new URLSearchParams(window.location.search).get("id");

  // Fetch workspace files.
  useEffect(() => {
    if (!sessionId) return;
    let cancelled = false;
    async function fetchFiles() {
      try {
        const res = await fetch(
          `/api/workspace?id=${encodeURIComponent(sessionId!)}`
        );
        if (res.ok) {
          const data = await res.json();
          const fileList = data.files || data || [];
          if (!cancelled) setFiles(fileList);
        }
      } catch {}
    }
    fetchFiles();
    const interval = setInterval(fetchFiles, 10000);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [sessionId]);

  // Initialize lean4monaco.
  useEffect(() => {
    if (!wrapperRef.current || !editorRef.current || !infoviewRef.current)
      return;

    let disposed = false;

    async function init() {
      // Load initial file content.
      let content = DEFAULT_CONTENT;
      if (sessionId) {
        try {
          const res = await fetch(
            `/api/editor/file?id=${encodeURIComponent(sessionId)}&path=Scratch.lean`
          );
          if (res.ok) {
            const data = await res.json();
            if (data.content) content = data.content;
          }
        } catch {}
      }
      if (disposed) return;

      const wsUrl = `ws://${window.location.host}/lean-ws`;
      const leanMonaco = new LeanMonaco();
      leanMonacoRef.current = leanMonaco;

      // Set infoview element BEFORE start.
      leanMonaco.setInfoviewElement(infoviewRef.current!);

      // Start with wrapper div as htmlElement (constrains Monaco UI).
      await leanMonaco.start({
        websocket: { url: wsUrl },
        htmlElement: wrapperRef.current!,
        vscode: {
          "workbench.colorTheme": "Visual Studio Dark",
        },
      });

      if (disposed) return;

      // Wait for ready.
      await leanMonaco.whenReady;
      if (disposed) return;
      setStatus("ready");

      // Create editor.
      const editor = new LeanMonacoEditor();
      leanEditorRef.current = editor;
      await editor.start(
        editorRef.current!,
        `LeanProject/${selectedFile}`,
        content
      );

      // Read-only: hover and diagnostics work, but no editing.
      editor.editor?.updateOptions({ readOnly: true });
    }

    init().catch((err) => {
      console.error("[editor] init failed:", err);
      setStatus("error");
    });

    return () => {
      disposed = true;
      leanEditorRef.current?.dispose();
      leanMonacoRef.current?.dispose();
    };
  }, [sessionId]);

  // Switch files -- dispose old editor and create new one with correct URI.
  const switchFile = async (path: string) => {
    if (!sessionId || !editorRef.current || !leanMonacoRef.current) return;
    setSelectedFile(path);
    try {
      const res = await fetch(
        `/api/editor/file?id=${encodeURIComponent(sessionId)}&path=${encodeURIComponent(path)}`
      );
      if (!res.ok) return;
      const data = await res.json();
      const content = data.content || "";

      // Dispose old editor and clear DOM, then create new one with correct URI.
      leanEditorRef.current?.dispose();
      while (editorRef.current?.firstChild) {
        editorRef.current.removeChild(editorRef.current.firstChild);
      }
      await leanMonacoRef.current.whenReady;
      const editor = new LeanMonacoEditor();
      leanEditorRef.current = editor;
      await editor.start(editorRef.current, `LeanProject/${path}`, content);
      editor.editor?.updateOptions({ readOnly: true });
    } catch {}
  };

  return (
    <div className="editor-root" ref={wrapperRef}>
      <div className="editor-toolbar">
        <span className="editor-title">Lean Editor</span>
        <span className={`editor-status editor-status--${status}`}>
          {status === "loading"
            ? "Loading Mathlib..."
            : status === "ready"
              ? "Connected"
              : "Error"}
        </span>
        <button
          className="editor-btn"
          onClick={() => leanMonacoRef.current?.restart()}
        >
          Restart Lean
        </button>
      </div>
      <div className="editor-body">
        {files.length > 1 && (
          <div className="editor-sidebar">
            {files.map((f) => (
              <button
                key={f.path}
                className={`editor-file-btn ${selectedFile === f.path ? "editor-file-btn--active" : ""}`}
                onClick={() => switchFile(f.path)}
              >
                {f.path}
              </button>
            ))}
          </div>
        )}
        <div className="editor-panes">
          <div className="editor-left" ref={editorRef} />
          <div className="editor-right" ref={infoviewRef} />
        </div>
      </div>
    </div>
  );
}
