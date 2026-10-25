import { api } from "./api";
import { useApi, useApp } from "./state";
import type { ScanView, Target } from "./types";
import { displayPath, fmtBytes, fmtTime } from "./util";
import { Ago, ErrorBox, Loading, Outcome, PathLink } from "./widgets";

function targetText(t: Target): string {
  const p = displayPath(t.path);
  if (t.kind === "file") return p;
  return `${p}${t.depth === 0 ? " (directory only)" : t.depth == null ? " (all below)" : ` (${t.depth} levels)`}`;
}

function scopeText(s: ScanView): string {
  const t = s.targets ?? [];
  if (t.length === 1) return targetText(t[0]);
  return `${t.length} targets`;
}

function duration(s: ScanView): string {
  if (!s.started_at) return "";
  const end = s.finished_at ?? Date.now();
  const ms = end - s.started_at;
  const sec = ms / 1000;
  if (ms < 1000) return `${ms} ms`;
  return sec < 60 ? `${sec.toFixed(1)} s` : `${Math.floor(sec / 60)} min ${Math.round(sec % 60)} s`;
}

export function Scans() {
  const { nav, go, status } = useApp();
  const scans = useApi(() => api.scans(), []);
  if (!scans.data) return scans.error ? <ErrorBox error={scans.error} /> : <Loading what="scans" />;
  return (
    <div className="split">
      <div className="split-main">
        <table className="entries">
          <thead>
            <tr>
              <th>#</th>
              <th>Operation</th>
              <th>Scope</th>
              <th>Outcome</th>
              <th>Requested</th>
              <th>Duration</th>
              <th className="num">Read</th>
              <th>Published</th>
              <th>Via</th>
            </tr>
          </thead>
          <tbody>
            {scans.data.map((s) => {
              const running = status?.busy?.scan_id === s.id;
              return (
                <tr key={s.id} className={nav.scan === s.id ? "sel" : ""} onClick={() => go({ scan: s.id })}>
                  <td>{s.id}</td>
                  <td>{s.operation}</td>
                  <td className="name">{scopeText(s)}</td>
                  <td>{running ? <span className="outcome oc-running">running</span> : <Outcome outcome={s.outcome} state={s.state} />}</td>
                  <td>
                    <Ago ms={s.requested_at} />
                  </td>
                  <td>{duration(s)}</td>
                  <td className="num">{s.summary?.counts ? fmtBytes(s.summary.counts.bytes_read) : ""}</td>
                  <td>{s.publication_seq != null ? `#${s.publication_seq}` : <span className="muted">no</span>}</td>
                  <td className="muted">{s.advisor ?? "direct"}</td>
                </tr>
              );
            })}
            {scans.data.length === 0 && (
              <tr>
                <td colSpan={9} className="muted pad">
                  No scans yet.
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
      <aside className="split-side">{nav.scan != null ? <ScanPanel id={nav.scan} /> : <div className="muted pad">Select a scan.</div>}</aside>
    </div>
  );
}

const COUNT_LABELS: Record<string, string> = {
  dirs_listed: "directories listed",
  entries: "entries observed",
  files_verified: "files fully read and recognized",
  blobs_new: "new Blobs admitted",
  blobs_recognized: "existing Blobs recognized",
  sample_match: "sample matches",
  sample_mismatch: "sample mismatches",
  size_mismatch: "size mismatches",
  no_baseline: "unresolved (nothing to compare)",
  unreadable: "unreadable",
  unstable: "unstable reads",
  not_regular: "not regular files",
  integrity_errors: "integrity errors",
  location_unconfirmed: "location unconfirmed",
  listing_failures: "listing failures",
  listing_retries: "listing retries",
  read_retries: "read retries",
  boundaries: "mount boundaries",
  symlinks: "symlinks",
  other_types: "special files",
};

function ScanPanel({ id }: { id: number }) {
  const { status, toast, pollNow } = useApp();
  const d = useApi(() => api.scan(id), [id]);
  if (!d.data) return d.error ? <ErrorBox error={d.error} /> : <Loading what={`scan #${id}`} />;
  const s = d.data.scan;
  const running = status?.busy?.scan_id === s.id;
  const counts = s.summary?.counts ?? {};
  const cancel = async () => {
    const r = await api.cancelScan(s.id);
    toast(r.cancelled === "not_found" ? "The scan is no longer running" : `Cancellation requested (${r.cancelled})`);
    pollNow();
  };
  return (
    <div className="detail">
      <h3>
        Scan #{s.id}: {s.operation}
      </h3>
      <p>
        {running ? <span className="outcome oc-running">running</span> : <Outcome outcome={s.outcome} state={s.state} />}{" "}
        {s.publication_seq != null ? `published as #${s.publication_seq}` : s.state === "closed" ? "not published: none of its evidence is in the current view" : ""}
      </p>
      {(running || s.state === "queued") && <button onClick={cancel}>Cancel</button>}
      {s.error && <ErrorBox error={s.error} />}
      <table className="facts">
        <tbody>
          <tr>
            <th>Requested</th>
            <td>{fmtTime(s.requested_at)}</td>
          </tr>
          <tr>
            <th>Ran</th>
            <td>
              {fmtTime(s.started_at)} {duration(s) && `(${duration(s)})`}
            </td>
          </tr>
          <tr>
            <th>Via</th>
            <td>{s.advisor ? `accepted suggestion: ${s.suggestion} (${s.advisor})` : "direct request"}</td>
          </tr>
          <tr>
            <th>Scanner</th>
            <td className="small mono">{s.scanner_version}</td>
          </tr>
          {s.policy && (
            <tr>
              <th>Policy</th>
              <td className="small mono">
                {String(s.policy.sampling ?? "no sampling")}; retry limit {String(s.policy.retry_limit)}
              </td>
            </tr>
          )}
        </tbody>
      </table>
      <div className="card">
        <div className="card-head">
          <b>Targets</b>
        </div>
        <ul className="plain">
          {(s.summary?.targets ?? s.targets.map((t) => ({ ...t, status: s.state }))).map((t, i) => (
            <li key={i}>
              {targetText(t as Target)} — <span className={`tstatus ts-${t.status}`}>{t.status.replaceAll("_", " ")}</span>
              {"reason" in t && t.reason ? <span className="muted"> ({t.reason})</span> : null}
            </li>
          ))}
        </ul>
      </div>
      {Object.keys(counts).length > 0 && (
        <div className="card">
          <div className="card-head">
            <b>Results</b> <span className="muted">{fmtBytes(counts.bytes_read)} of content read</span>
          </div>
          <table className="mini">
            <tbody>
              {Object.entries(counts)
                .filter(([k, v]) => v > 0 && k !== "bytes_read")
                .map(([k, v]) => (
                  <tr key={k}>
                    <td className="num">{v.toLocaleString()}</td>
                    <td>{COUNT_LABELS[k] ?? k}</td>
                  </tr>
                ))}
            </tbody>
          </table>
          {(s.summary?.gaps ?? 0) > 0 && (
            <p className="note">
              Finished with {s.summary?.gaps} gap(s): {Object.entries(s.summary?.gaps_by_code ?? {}).map(([k, v]) => `${k.replaceAll("_", " ")} ×${v}`).join(", ")}. Other work in this scan is
              still valid.
            </p>
          )}
        </div>
      )}
      {d.data.diagnostics.length > 0 && (
        <div className="card">
          <div className="card-head">
            <b>Diagnostics</b> <span className="muted">{d.data.diagnostics_total}</span>
          </div>
          <ul className="events">
            {d.data.diagnostics.map((g, i) => (
              <li key={i} className={g.severity === "info" ? "" : "bad"}>
                <span className="ev-when">{g.code}</span>
                {g.path != null && <PathLink path={g.path} />} <span className="ev-text">{g.message}</span>
              </li>
            ))}
          </ul>
        </div>
      )}
      {d.data.proposal && (
        <details className="card">
          <summary>
            <b>Accepted proposal</b> <span className="muted">(as reviewed)</span>
          </summary>
          <pre className="json">{JSON.stringify(d.data.proposal, null, 2)}</pre>
        </details>
      )}
    </div>
  );
}
