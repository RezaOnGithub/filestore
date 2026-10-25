import { useState } from "react";
import { api } from "./api";
import { LineageDialog, TagDialog } from "./dialogs";
import { useApi, useApp } from "./state";
import type { Entry } from "./types";
import { END_REASON, baseName, displayPath, exact, explain, fmtBytes, fmtTime } from "./util";
import { Ago, Badge, BlobLink, ErrorBox, Loading, PathLink, TagChips } from "./widgets";

export function Detail({ path }: { path: string }) {
  const { runScan, go, toast, pollNow } = useApp();
  const loc = useApi(() => api.location(path), [path]);
  const [dialog, setDialog] = useState<null | "tag" | "lineage">(null);
  if (!loc.data) return loc.error ? <ErrorBox error={loc.error} /> : <Loading what="evidence" />;
  const d = loc.data;
  const e: Entry | undefined = d.current ?? d.occurrences[0];
  if (!e) return <div className="muted pad">Nothing recorded at {displayPath(path)}.</div>;
  const isDir = e.kind === "dir";
  const target = isDir ? { path, kind: "dir" as const, depth: 0 } : { path, kind: "file" as const };
  const removeTag = async (hierarchy: string) => {
    try {
      await api.assign("blob", e.blob!, hierarchy, null);
      pollNow();
    } catch (err) {
      toast((err as Error).message, "error");
    }
  };

  return (
    <div className="detail">
      <h3 title={displayPath(path)}>
        {isDir ? "📁 " : ""}
        {baseName(path) || "root"}
      </h3>
      <div className="muted mono small">{displayPath(path)}</div>
      <p>
        <Badge status={e.status} issue={!!e.issue} /> {explain(e)}
      </p>
      <div className="toolbar">
        {isDir && e.present && <button onClick={() => go({ path, sel: null })}>Open</button>}
        <button onClick={() => runScan("inventory", [target])} title="Observe names and metadata only">
          Inventory
        </button>
        <button onClick={() => runScan("verify", [isDir ? { ...target, depth: null } : target])} title="Full read: exact content identity">
          Verify
        </button>
        <button onClick={() => runScan("check", [isDir ? { ...target, depth: null } : target])} title="Sampled comparison against known content">
          Check
        </button>
      </div>

      <table className="facts">
        <tbody>
          <tr>
            <th>Kind</th>
            <td>
              {e.kind ?? "unknown"}
              {e.boundary ? " (other filesystem)" : ""}
            </td>
          </tr>
          {!isDir && (
            <tr>
              <th>Size</th>
              <td>
                {fmtBytes(e.size)} <span className="muted">({e.size?.toLocaleString()} bytes)</span>
              </td>
            </tr>
          )}
          <tr>
            <th>Modified</th>
            <td>{fmtTime(e.mtime_ms)}</td>
          </tr>
          <tr>
            <th>Inode</th>
            <td className="mono">
              {e.ino ?? "—"} <span className="muted">epoch {e.epoch}</span>
            </td>
          </tr>
          <tr>
            <th>Seen</th>
            <td>
              first <Ago ms={e.first_seen_at} />, last <Ago ms={e.last_seen_at} /> (publication #{e.last_seen_seq})
            </td>
          </tr>
          {!isDir && (
            <>
              <tr>
                <th>Verified</th>
                <td>
                  {e.verified_at ? (
                    <>
                      <Ago ms={e.verified_at} /> as <BlobLink id={e.verified_blob} />
                    </>
                  ) : (
                    "never at this occurrence"
                  )}
                </td>
              </tr>
              <tr>
                <th>Last check</th>
                <td>
                  {e.check ? (
                    <>
                      {e.check.result.replace("_", " ")} <Ago ms={e.check.at} />
                      {e.check.blob && (
                        <>
                          {" "}
                          vs <BlobLink id={e.check.blob} />
                        </>
                      )}
                    </>
                  ) : (
                    "—"
                  )}
                </td>
              </tr>
            </>
          )}
          {isDir && e.listing && (
            <tr>
              <th>Listing</th>
              <td>
                {e.listing.listed_at ? (
                  <>
                    complete <Ago ms={e.listing.listed_at} />, {e.listing.entries} entries{e.listing.stale ? " — stale" : ""}
                  </>
                ) : (
                  "never listed"
                )}
              </td>
            </tr>
          )}
        </tbody>
      </table>

      {!isDir && e.blob && (
        <div className="card">
          <div className="card-head">
            <b>Content</b> <BlobLink id={e.blob} full /> <span className="muted">{e.format}</span>
          </div>
          {!exact(e) && (
            <p className="note">
              This file is <b>{e.status}</b>, not verified: the content shown is what it {e.status === "changed" ? "was ruled out against" : "is expected to be"}. Verify before tagging it or using it as a
              revision.
            </p>
          )}
          <div>
            Tags: {e.tags.length ? <TagChips tags={e.tags} dim={!exact(e)} onRemove={exact(e) ? (t) => removeTag(t.hierarchy) : undefined} /> : <span className="muted">none</span>}
          </div>
          <div>
            Lineages:{" "}
            {d.revisions.length ? (
              d.revisions.map((r) => (
                <a
                  key={r.revision}
                  href="#"
                  className="lin-link"
                  onClick={(ev) => {
                    ev.preventDefault();
                    go({ tab: "lineages", lineage: r.lineage });
                  }}
                >
                  {r.lineage_title}
                  {r.label ? ` @ ${r.label}` : ""}
                </a>
              ))
            ) : (
              <span className="muted">none</span>
            )}
          </div>
          <div className="toolbar">
            <button disabled={!exact(e)} onClick={() => setDialog("tag")}>
              Tag…
            </button>
            <button disabled={!exact(e)} onClick={() => setDialog("lineage")}>
              Add to Lineage…
            </button>
          </div>
        </div>
      )}
      {!isDir && !e.blob && e.present && (
        <div className="card">
          <p className="note">Content unidentified: there is no Blob to tag or record as a revision. Verify this file first.</p>
        </div>
      )}

      {d.same_inode.length > 0 && (
        <div className="card">
          <div className="card-head">
            <b>Same inode elsewhere</b> <span className="muted">(a possible move or hard link, not proof)</span>
          </div>
          <ul className="plain">
            {d.same_inode.map((s) => (
              <li key={s.path}>
                <PathLink path={s.path} /> <span className="muted">{s.present ? s.status : `gone ${s.ended_at ? new Date(s.ended_at).toLocaleString() : ""}`}</span>
              </li>
            ))}
          </ul>
        </div>
      )}

      {d.occurrences.length > 1 && (
        <div className="card">
          <div className="card-head">
            <b>Occurrences at this path</b> <span className="muted">(newest first)</span>
          </div>
          <ul className="plain occ-list">
            {d.occurrences.map((o, i) => (
              <li key={i}>
                <Badge status={o.status} /> <span className="mono small">inode {o.ino ?? "?"}</span>{" "}
                <span className="small">
                  <Ago ms={o.first_seen_at} /> → {o.ended_at ? <Ago ms={o.ended_at} /> : "now"}
                </span>{" "}
                {(o.verified_blob ?? o.blob) && <BlobLink id={o.verified_blob ?? o.blob} />}
                {o.end_reason && <div className="muted small">ended: {END_REASON[o.end_reason] ?? o.end_reason}</div>}
              </li>
            ))}
          </ul>
        </div>
      )}

      <div className="card">
        <div className="card-head">
          <b>Evidence</b> <span className="muted">(observations and attempts, newest first)</span>
        </div>
        <ul className="events">
          {d.events.map((ev, i) => (
            <li key={i} className={ev.ok === false ? "bad" : ev.ok ? "good" : ""}>
              <span className="ev-when" title={fmtTime(ev.at)}>
                <Ago ms={ev.at} />
              </span>
              <a
                href="#"
                className="ev-scan"
                onClick={(x) => {
                  x.preventDefault();
                  go({ tab: "scans", scan: ev.scan });
                }}
              >
                #{ev.scan} {ev.operation}
              </a>
              {!ev.published && <span className="unpub" title="This scan was not published; its evidence is history only">{ev.outcome ?? "unpublished"}</span>}
              <span className="ev-text">{ev.summary}</span>
            </li>
          ))}
        </ul>
      </div>
      {dialog === "tag" && <TagDialog targets={[{ kind: "blob", id: e.blob!, label: e.name }]} onClose={() => setDialog(null)} />}
      {dialog === "lineage" && <LineageDialog items={[{ blob: e.blob!, path, name: e.name }]} onClose={() => setDialog(null)} />}
    </div>
  );
}
