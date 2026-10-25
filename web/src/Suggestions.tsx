import { useState } from "react";
import { ApiError, api } from "./api";
import { ReviewDialog } from "./dialogs";
import { useApi, useApp } from "./state";
import type { Suggestion, Target } from "./types";
import { displayPath, plural } from "./util";
import { ErrorBox, Loading, PathLink } from "./widgets";

const DISMISSED = "filestore.dismissed";

function loadDismissed(): Set<string> {
  try {
    return new Set(JSON.parse(sessionStorage.getItem(DISMISSED) ?? "[]"));
  } catch {
    return new Set();
  }
}

function describeTargets(t: Target[]): string {
  const dirs = t.filter((x) => x.kind === "dir");
  const files = t.length - dirs.length;
  const parts = [];
  if (dirs.length)
    parts.push(
      dirs.length === 1
        ? `${dirs[0].path === "" ? "the root" : displayPath(dirs[0].path)}${dirs[0].depth === 0 ? " (directory only)" : dirs[0].depth == null ? " and everything below" : ""}`
        : plural(dirs.length, "directory", "directories"),
    );
  if (files) parts.push(plural(files, "file"));
  return parts.join(" and ");
}

export function Suggestions() {
  const sugg = useApi(() => api.suggestions(), []);
  const [dismissed, setDismissed] = useState(loadDismissed);
  const [review, setReview] = useState<Suggestion | null>(null);
  const [showDismissed, setShowDismissed] = useState(false);
  if (!sugg.data) return sugg.error ? <ErrorBox error={sugg.error} /> : <Loading what="suggestions" />;
  const dismiss = (id: string) => {
    const n = new Set(dismissed);
    n.add(id);
    setDismissed(n);
    sessionStorage.setItem(DISMISSED, JSON.stringify([...n]));
  };
  const visible = sugg.data.filter((s) => showDismissed || !dismissed.has(s.id));
  const scans = visible.filter((s) => s.action.type === "scan");
  const semantic = visible.filter((s) => s.action.type !== "scan");
  const hidden = sugg.data.length - visible.length;
  return (
    <div className="page">
      <p className="muted">
        Advisors read the catalog (never file content) and propose actions. Nothing happens until you accept; accepting records the reviewed proposal with the resulting scan or change.
        {hidden > 0 && (
          <>
            {" "}
            {plural(hidden, "suggestion")} dismissed for this session —{" "}
            <a
              href="#"
              onClick={(e) => {
                e.preventDefault();
                setShowDismissed(true);
              }}
            >
              show
            </a>
            .
          </>
        )}
      </p>
      <h2>Scans {scans.length > 0 && <span className="count">{scans.length}</span>}</h2>
      {scans.length === 0 && <p className="muted">No scan suggestions: every known location has current evidence.</p>}
      {scans.map((s) => (
        <Card key={s.id} s={s} onDismiss={() => dismiss(s.id)} onReview={() => setReview(s)} reload={sugg.reload} dismissed={dismissed.has(s.id)} />
      ))}
      <h2>Semantic proposals {semantic.length > 0 && <span className="count">{semantic.length}</span>}</h2>
      {semantic.length === 0 && <p className="muted">No proposals. Lineage suggestions appear when verified content changes at a path, or for files whose names look like versions.</p>}
      {semantic.map((s) => (
        <Card key={s.id} s={s} onDismiss={() => dismiss(s.id)} onReview={() => setReview(s)} reload={sugg.reload} dismissed={dismissed.has(s.id)} />
      ))}
      {review && (
        <ReviewDialog
          s={review}
          onClose={() => setReview(null)}
          onDone={() => {
            setReview(null);
            sugg.reload();
          }}
        />
      )}
    </div>
  );
}

function Card({ s, onDismiss, onReview, reload, dismissed }: { s: Suggestion; onDismiss(): void; onReview(): void; reload(): void; dismissed: boolean }) {
  const { toast, pollNow, busy } = useApp();
  const [open, setOpen] = useState(false);
  const [pending, setPending] = useState(false);
  const accept = async () => {
    setPending(true);
    try {
      const r = await api.acceptSuggestion(s.id);
      toast(`Accepted: scan #${r.id} queued`);
      pollNow();
      reload();
    } catch (e) {
      toast((e as Error).message, "error");
      if (e instanceof ApiError && e.status === 409) reload();
    } finally {
      setPending(false);
    }
  };
  const shown = open ? s.evidence : s.evidence.slice(0, 5);
  return (
    <div className={dismissed ? "suggestion dismissed" : "suggestion"}>
      <div className="sg-head">
        <b>{s.title}</b>
        <span className="sg-advisor" title={`advisor ${s.advisor} v${s.advisor_version}, computed at publication #${s.input_seq}`}>
          {s.advisor}
        </span>
      </div>
      <p>{s.reason}</p>
      <p className="note">
        <b>Uncertainty:</b> {s.uncertainty} <b>Resolves:</b> {s.resolves}
      </p>
      {s.action.type === "scan" && (
        <p className="small">
          <b>Action:</b> {s.action.operation} {describeTargets(s.action.targets)}
        </p>
      )}
      {s.evidence.length > 0 && (
        <ul className="evidence">
          {shown.map((e, i) => (
            <li key={i}>
              <PathLink path={e.path} /> <span className="muted">— {e.detail}</span>
            </li>
          ))}
          {s.evidence_total > shown.length && (
            <li>
              <a
                href="#"
                onClick={(e) => {
                  e.preventDefault();
                  setOpen(!open);
                }}
              >
                {open ? (s.evidence_total > s.evidence.length ? `… ${s.evidence_total - s.evidence.length} more not listed` : "show fewer") : `show ${s.evidence_total - shown.length} more`}
              </a>
            </li>
          )}
        </ul>
      )}
      <div className="toolbar">
        {s.action.type === "scan" ? (
          <button className="primary" disabled={pending} onClick={accept} title={busy ? "Will be recorded when the running scan finishes" : undefined}>
            Accept: {s.action.operation}
          </button>
        ) : (
          <button className="primary" onClick={onReview}>
            Review…
          </button>
        )}
        {!dismissed && <button onClick={onDismiss}>Dismiss</button>}
      </div>
    </div>
  );
}
