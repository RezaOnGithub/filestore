import { useState } from "react";
import { api } from "./api";
import { LineageDialog, TagDialog } from "./dialogs";
import { useApi, useApp } from "./state";
import { END_REASON, baseName, fmtBytes, fmtTime } from "./util";
import type { Status } from "./types";
import { Ago, Badge, BlobLink, ErrorBox, Loading, PathLink, TagChips } from "./widgets";

const FILTERS: [string, string][] = [
  ["all", "all content"],
  ["duplicates", "present at 2+ paths"],
  ["missing", "no current location"],
  ["tagged", "tagged"],
  ["lineage", "in a Lineage"],
];

export function Blobs() {
  const { nav, go } = useApp();
  const [filter, setFilter] = useState("all");
  const [text, setText] = useState("");
  const [query, setQuery] = useState("");
  const list = useApi(() => api.blobs(filter, query), [filter, query]);
  return (
    <div className="split">
      <div className="split-main">
        <div className="toolbar">
          <select value={filter} onChange={(e) => setFilter(e.target.value)}>
            {FILTERS.map(([v, l]) => (
              <option key={v} value={v}>
                {l}
              </option>
            ))}
          </select>
          <form
            onSubmit={(e) => {
              e.preventDefault();
              setQuery(text);
            }}
          >
            <input placeholder="Search Blob ID or current path…" value={text} onChange={(e) => setText(e.target.value)} />
            <button type="submit">Search</button>
          </form>
          <span className="muted small">
            A Blob is one exact byte sequence. It persists even when no file currently holds it. {list.data && `${list.data.length} shown.`}
          </span>
        </div>
        {!list.data ? (
          list.error ? <ErrorBox error={list.error} /> : <Loading what="Blobs" />
        ) : (
          <div className="table-wrap">
            <table className="entries">
              <thead>
                <tr>
                  <th>Blob</th>
                  <th className="num">Size</th>
                  <th>Format</th>
                  <th className="num">Present</th>
                  <th>Current paths</th>
                  <th>Tags</th>
                  <th>Lineages</th>
                </tr>
              </thead>
              <tbody>
                {list.data.map((b) => (
                  <tr key={b.id} className={nav.blob === b.id ? "sel" : ""} onClick={() => go({ blob: b.id })}>
                    <td>
                      <BlobLink id={b.id} />
                    </td>
                    <td className="num">{fmtBytes(b.size)}</td>
                    <td className="muted">{b.format}</td>
                    <td className="num">{b.present || <span className="warn">0</span>}</td>
                    <td className="name" title={b.paths.join("\n")}>
                      {b.paths[0] ? baseName(b.paths[0]) : <span className="muted">none</span>}
                      {b.paths.length > 1 && <span className="muted"> +{b.paths.length - 1}</span>}
                    </td>
                    <td>
                      <TagChips tags={b.tags} />
                    </td>
                    <td>{b.lineages || ""}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>
      <aside className="split-side">{nav.blob ? <BlobPanel id={nav.blob} /> : <div className="muted pad">Select a Blob.</div>}</aside>
    </div>
  );
}

export function BlobPanel({ id }: { id: string }) {
  const { go, toast, pollNow } = useApp();
  const b = useApi(() => api.blob(id), [id]);
  const [dialog, setDialog] = useState<null | "tag" | "lineage">(null);
  if (!b.data) return b.error ? <ErrorBox error={b.error} /> : <Loading what="Blob" />;
  const d = b.data;
  const present = d.occurrences.filter((o) => o.present && (o.status === "verified" || o.status === "plausible" || o.status === "modified"));
  const anyPath = present[0]?.path ?? d.occurrences[0]?.path ?? d.admitted_path;
  const removeTag = async (hierarchy: string) => {
    try {
      await api.assign("blob", d.id, hierarchy, null);
      pollNow();
    } catch (e) {
      toast((e as Error).message, "error");
    }
  };
  return (
    <div className="detail">
      <h3 className="mono">{d.id}</h3>
      <p>
        {fmtBytes(d.size)} <span className="muted">({d.size.toLocaleString()} bytes)</span>. First admitted <Ago ms={d.created_at} /> by scan #{d.admitted_scan} from{" "}
        <PathLink path={d.admitted_path} />. Recognized by {d.recognitions} full read(s).
      </p>
      <p className="muted small">
        Its BLAKE3 content digest and sample baselines stay private to the catalog; this random ID is the only name used outside it.
      </p>
      <div className="card">
        <div className="card-head">
          <b>Tags</b>
        </div>
        {d.tags.length ? <TagChips tags={d.tags} onRemove={(t) => removeTag(t.hierarchy)} /> : <span className="muted">none</span>}
        <div className="toolbar">
          <button onClick={() => setDialog("tag")}>Tag…</button>
          <button onClick={() => setDialog("lineage")}>Add to Lineage…</button>
        </div>
      </div>
      <div className="card">
        <div className="card-head">
          <b>Revisions</b>
        </div>
        {d.revisions.length === 0 && <span className="muted">Not a revision in any Lineage.</span>}
        <ul className="plain">
          {d.revisions.map((r) => (
            <li key={r.revision}>
              <a
                href="#"
                onClick={(e) => {
                  e.preventDefault();
                  go({ tab: "lineages", lineage: r.lineage });
                }}
              >
                {r.lineage_title}
              </a>{" "}
              {r.label && <span className="muted">@ {r.label}</span>}
            </li>
          ))}
        </ul>
      </div>
      <div className="card">
        <div className="card-head">
          <b>Locations</b> <span className="muted">(current first)</span>
        </div>
        <table className="mini">
          <tbody>
            {d.occurrences.map((o, i) => (
              <tr key={i} className={o.present ? "" : "gone"}>
                <td className="name">
                  <PathLink path={o.path} />
                </td>
                <td>
                  <Badge status={o.status as Status} />
                </td>
                <td className="small">
                  {o.verified_at ? (
                    <>
                      verified <Ago ms={o.verified_at} />
                    </>
                  ) : o.content_at ? (
                    <>
                      evidence <Ago ms={o.content_at} />
                    </>
                  ) : (
                    ""
                  )}
                </td>
                <td className="muted small">{o.end_reason ? END_REASON[o.end_reason] ?? o.end_reason : ""}</td>
              </tr>
            ))}
          </tbody>
        </table>
        {present.length === 0 && <p className="note">No current location holds this content as far as the evidence shows. That does not prove it is gone everywhere.</p>}
      </div>
      <div className="card">
        <div className="card-head">
          <b>Derived metadata</b>
        </div>
        {d.metadata.map((m, i) => (
          <div key={i} className="small">
            <span className="mono">
              {m.extractor}/{m.version}
            </span>{" "}
            <span className="mono">{JSON.stringify(m.payload)}</span> <span className="muted">scan #{m.scan}, {fmtTime(m.created_at)}</span>
          </div>
        ))}
        {d.baselines.map((p) => (
          <div key={p} className="small muted">
            sample baseline under <span className="mono">{p}</span>
          </div>
        ))}
        {d.metadata.length === 0 && d.baselines.length === 0 && <span className="muted">none</span>}
      </div>
      {dialog === "tag" && <TagDialog targets={[{ kind: "blob", id: d.id, label: d.id }]} onClose={() => setDialog(null)} />}
      {dialog === "lineage" && <LineageDialog items={[{ blob: d.id, path: anyPath, name: baseName(anyPath) }]} onClose={() => setDialog(null)} />}
    </div>
  );
}
