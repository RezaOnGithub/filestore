import { useState } from "react";
import { api } from "./api";
import { useApi, useApp } from "./state";
import { fmtTime } from "./util";
import { Ago, ErrorBox, Loading } from "./widgets";

export function History() {
  const h = useApi(() => api.history(), []);
  if (!h.data) return h.error ? <ErrorBox error={h.error} /> : <Loading what="history" />;
  return (
    <div className="page">
      <p className="muted">Every semantic change is an operation with before/after values. Scan evidence is never rewritten by these.</p>
      <table className="entries">
        <thead>
          <tr>
            <th>When</th>
            <th>Operation</th>
            <th>Summary</th>
            <th>Via</th>
            <th>Changes</th>
          </tr>
        </thead>
        <tbody>
          {h.data.map((o) => (
            <tr key={o.id}>
              <td title={fmtTime(o.created_at)}>
                <Ago ms={o.created_at} />
              </td>
              <td className="mono small">{o.kind}</td>
              <td>{o.summary}</td>
              <td className="muted">{o.advisor ? `suggestion (${o.advisor})` : o.actor}</td>
              <td>
                <details>
                  <summary className="small">details</summary>
                  <pre className="json">{JSON.stringify(o.changes, null, 2)}</pre>
                </details>
              </td>
            </tr>
          ))}
          {h.data.length === 0 && (
            <tr>
              <td colSpan={5} className="muted pad">
                No semantic changes yet.
              </td>
            </tr>
          )}
        </tbody>
      </table>
    </div>
  );
}

export function Settings() {
  const { status, toast, pollNow } = useApp();
  const info = status?.info;
  const [days, setDays] = useState<string>("");
  if (!info) return <Loading />;
  const save = async () => {
    try {
      await api.setHorizon(Number(days));
      toast("Saved");
      setDays("");
      pollNow();
    } catch (e) {
      toast((e as Error).message, "error");
    }
  };
  return (
    <div className="page">
      <h2>Catalog</h2>
      <table className="facts">
        <tbody>
          <tr>
            <th>Catalog</th>
            <td className="mono">{info.catalog}</td>
          </tr>
          <tr>
            <th>Scan root</th>
            <td className="mono">{info.root}</td>
          </tr>
          <tr>
            <th>Archive</th>
            <td className="mono">{info.archive_id}</td>
          </tr>
          <tr>
            <th>Identity epoch</th>
            <td>
              {info.epoch ? (
                <>
                  #{info.epoch.number} since {fmtTime(info.epoch.created_at)} ({info.epoch.reason}); filesystem <span className="mono">{info.epoch.fs_context}</span>, root inode{" "}
                  {info.epoch.root_ino}
                </>
              ) : (
                "none yet (created by the first scan)"
              )}
            </td>
          </tr>
          <tr>
            <th>Contents</th>
            <td>
              {info.scans} scans, {info.blobs} Blobs, {info.semantic_ops} semantic operations; latest publication #{info.publication_seq}
            </td>
          </tr>
        </tbody>
      </table>
      <h2>Verification horizon</h2>
      <p>
        Verified files are suggested for re-verification after <b>{info.verify_horizon_days} days</b>. Sample checks never extend this; overdue does not mean absent.
      </p>
      <form
        className="inline-form"
        onSubmit={(e) => {
          e.preventDefault();
          save();
        }}
      >
        <input type="number" min={1} placeholder={String(info.verify_horizon_days)} value={days} onChange={(e) => setDays(e.target.value)} />
        <span>days</span>
        <button type="submit" disabled={!days}>
          Save
        </button>
      </form>
      <h2>How the operations work</h2>
      <ul>
        <li>
          <b>Inventory</b> lists directories and records metadata (inode, size, timestamps). It reads no content and never renews a verification.
        </li>
        <li>
          <b>Verify</b> reads every byte, hashes it (BLAKE3, kept private), and recognizes the content as a Blob with a random public ID. The file must stay stable while read, and its path is
          re-resolved afterwards to confirm the location.
        </li>
        <li>
          <b>Check</b> compares a file with its expected content. Files up to 1 MiB are fully read (a Verify). Larger files are sampled: 16 evenly spaced 64 KiB blocks, always including the first
          and last, compared with a baseline stored at verification. A match makes the content <i>plausible</i>; a size or sample difference rules it out.
        </li>
      </ul>
      <p className="muted small">
        Symlinks are recorded but never followed; mount points are not crossed. A scan has exclusive use of the catalog while it runs, so other views wait and show their last loaded data.
      </p>
    </div>
  );
}
