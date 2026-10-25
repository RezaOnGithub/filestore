import { useState } from "react";
import { api } from "./api";
import { Blobs } from "./Blobs";
import { Browse } from "./Browse";
import { Lineages } from "./Lineages";
import { History, Settings } from "./Misc";
import { Scans } from "./Scans";
import { Suggestions } from "./Suggestions";
import { Tags } from "./Tags";
import { AppProvider, useApi, useApp } from "./state";
import type { Tab } from "./state";
import { fmtBytes } from "./util";
import { Ago, Modal } from "./widgets";

export function App() {
  return (
    <AppProvider>
      <Shell />
    </AppProvider>
  );
}

const TABS: [Tab, string][] = [
  ["browse", "Browse"],
  ["suggestions", "Suggestions"],
  ["scans", "Scans"],
  ["blobs", "Blobs"],
  ["lineages", "Lineages"],
  ["tags", "Tags"],
  ["history", "History"],
  ["settings", "Settings"],
];

function Shell() {
  const { nav, go, status, toasts } = useApp();
  const sugg = useApi(() => api.suggestions(), []);
  const scanCount = sugg.data?.filter((s) => s.action.type === "scan").length ?? 0;
  const semCount = (sugg.data?.length ?? 0) - scanCount;
  return (
    <div className="app">
      <header>
        <span className="logo">Filestore</span>
        <span className="root mono" title={`catalog: ${status?.info?.catalog ?? ""}`}>
          {status?.info?.root ?? "…"}
        </span>
        <StatusArea />
      </header>
      <BusyBar />
      <RootBanner />
      {status && status.recovered > 0 && (
        <div className="banner info">
          {status.recovered} scan(s) were interrupted by a previous process exit and are recorded as abandoned; their partial evidence is history only.
        </div>
      )}
      <nav className="tabs">
        {TABS.map(([t, label]) => (
          <button key={t} className={nav.tab === t ? "tab active" : "tab"} onClick={() => go({ tab: t })}>
            {label}
            {t === "suggestions" && scanCount > 0 && <span className="count">{scanCount}</span>}
            {t === "suggestions" && semCount > 0 && (
              <span className="count sem" title="semantic proposals">
                {semCount}
              </span>
            )}
          </button>
        ))}
      </nav>
      <main>
        {status === null && <div className="banner error">Cannot reach the Filestore server.</div>}
        {nav.tab === "browse" && <Browse />}
        {nav.tab === "suggestions" && <Suggestions />}
        {nav.tab === "scans" && <Scans />}
        {nav.tab === "blobs" && <Blobs />}
        {nav.tab === "lineages" && <Lineages />}
        {nav.tab === "tags" && <Tags />}
        {nav.tab === "history" && <History />}
        {nav.tab === "settings" && <Settings />}
      </main>
      <div className="toasts">
        {toasts.map((t) => (
          <div key={t.id} className={`toast ${t.kind}`}>
            {t.text}
          </div>
        ))}
      </div>
    </div>
  );
}

function StatusArea() {
  const { status } = useApp();
  if (!status) return <span className="status-area" />;
  const last = status.last_scan;
  const queued = status.queued.length;
  return (
    <span className="status-area">
      {status.busy ? <span className="spinner" /> : <span className="idle-dot" />}
      {status.busy ? "scanning" : "idle"} · publication #{status.info?.publication_seq ?? 0}
      {last && (
        <span className="muted">
          · last scan #{last.scan_id} {last.outcome.replaceAll("_", " ")}
        </span>
      )}
      {queued > 0 && <span>· {queued} queued</span>}
      {status.info?.epoch && (
        <span className="muted" title={status.info.epoch.fs_context}>
          · epoch #{status.info.epoch.number}
        </span>
      )}
    </span>
  );
}

/** Progress of the running scan, which holds the catalog exclusively. */
function BusyBar() {
  const { status, go, toast, pollNow } = useApp();
  const p = status?.busy;
  if (!p) return null;
  const cancel = async () => {
    const r = await api.cancelScan(p.scan_id);
    toast(`Cancellation of scan #${p.scan_id}: ${r.cancelled}`);
    pollNow();
  };
  return (
    <div className="busy-bar">
      <span className="spinner dark" />
      <a href="#" onClick={(e) => (e.preventDefault(), go({ tab: "scans", scan: p.scan_id }))}>
        Scan #{p.scan_id} {p.operation ?? ""}
      </a>
      <span>
        {p.dirs_listed} dirs listed · {p.entries_seen} entries · {p.files_read} files read · {fmtBytes(p.bytes_read)}
        {p.problems ? ` · ${p.problems} problem(s)` : ""} · started <Ago ms={p.started_at} />
      </span>
      <span className="current mono" title={p.current}>
        {p.current}
      </span>
      <button onClick={cancel}>Cancel</button>
      <span className="muted small">The scan has exclusive use of the catalog; other views wait and show data as of publication #{status?.info?.publication_seq}.</span>
    </div>
  );
}

function RootBanner() {
  const { status, toast, pollNow } = useApp();
  const [confirm, setConfirm] = useState(false);
  const pending = status?.info?.pending_root_change;
  if (!pending) return null;
  const adopt = async () => {
    try {
      await api.adoptRoot();
      toast("Adopted the new root identity; a new identity epoch has started");
      setConfirm(false);
      pollNow();
    } catch (e) {
      toast((e as Error).message, "error");
    }
  };
  return (
    <div className="banner caution">
      <b>Scans are blocked:</b> {pending.message}. The archive is not treated as deleted. <button onClick={() => setConfirm(true)}>Review adoption…</button>
      {confirm && (
        <Modal title="Adopt the new root identity?" onClose={() => setConfirm(false)}>
          <p>
            Previously: <span className="mono">{pending.previous_fs_context}</span>, root inode {pending.previous_root_ino}.<br />
            Now: <span className="mono">{pending.fs_context}</span>, root inode {pending.root_ino}.
          </p>
          <p>Adopting starts a new identity epoch. Earlier observations become history: no directory listing or verification carries over, because inode continuity cannot be assumed.</p>
          <p>
            Content identity does carry over: <b>Check</b> compares files with the content last verified at the same paths (plausible matches), and <b>Verify</b> recovers existing Blob IDs, tags, and
            revisions.
          </p>
          <p className="muted small">This prototype offers no candidate comparison report before adoption (see README).</p>
          <div className="modal-foot">
            <button onClick={() => setConfirm(false)}>Cancel</button>
            <button className="primary" onClick={adopt}>
              Adopt
            </button>
          </div>
        </Modal>
      )}
    </div>
  );
}
