import { useState } from "react";
import { api } from "./api";
import { TagDialog } from "./dialogs";
import type { TagTarget } from "./dialogs";
import { useApi, useApp } from "./state";
import type { Entry, LineageView, RevisionView } from "./types";
import { baseName, displayPath, fmtBytes, shortId } from "./util";
import { Ago, BlobLink, ErrorBox, Loading, Modal, PathLink, TagChips } from "./widgets";

export function Lineages() {
  const { nav, go, toast } = useApp();
  const list = useApi(() => api.lineages(), []);
  const [creating, setCreating] = useState(false);
  const [title, setTitle] = useState("");
  const create = async () => {
    try {
      const r = await api.createLineage(title, []);
      setCreating(false);
      setTitle("");
      go({ lineage: r.id });
      list.reload();
    } catch (e) {
      toast((e as Error).message, "error");
    }
  };
  return (
    <div className="split">
      <div className="split-main narrow">
        <div className="toolbar">
          {creating ? (
            <form
              onSubmit={(e) => {
                e.preventDefault();
                create();
              }}
            >
              <input autoFocus placeholder="Lineage title" value={title} onChange={(e) => setTitle(e.target.value)} />
              <button type="submit" className="primary" disabled={!title.trim()}>
                Create
              </button>
              <button type="button" onClick={() => setCreating(false)}>
                Cancel
              </button>
            </form>
          ) : (
            <button onClick={() => setCreating(true)}>New Lineage…</button>
          )}
        </div>
        <p className="muted small">A Lineage is your record that several exact contents are revisions of one thing. Its revisions survive renames, edits, and deletions of the files.</p>
        {!list.data ? (
          list.error ? <ErrorBox error={list.error} /> : <Loading what="Lineages" />
        ) : (
          <table className="entries">
            <tbody>
              {list.data.map((l) => (
                <tr key={l.id} className={nav.lineage === l.id ? "sel" : ""} onClick={() => go({ lineage: l.id })}>
                  <td className="name">{l.title}</td>
                  <td className="num">{l.revisions}</td>
                  <td>
                    <TagChips tags={l.tags} />
                  </td>
                </tr>
              ))}
              {list.data.length === 0 && (
                <tr>
                  <td className="muted pad">No Lineages yet. Create one here, from selected verified files in Browse, or by accepting a semantic proposal.</td>
                </tr>
              )}
            </tbody>
          </table>
        )}
      </div>
      <aside className="split-side wide">{nav.lineage ? <LineagePanel id={nav.lineage} /> : <div className="muted pad">Select a Lineage.</div>}</aside>
    </div>
  );
}

function depthOf(revs: RevisionView[]): Map<string, number> {
  const byId = new Map(revs.map((r) => [r.id, r]));
  const memo = new Map<string, number>();
  const depth = (id: string, guard = 0): number => {
    if (memo.has(id)) return memo.get(id)!;
    const r = byId.get(id);
    const d = !r || r.parents.length === 0 || guard > 1000 ? 0 : 1 + Math.max(...r.parents.map((p) => depth(p, guard + 1)));
    memo.set(id, d);
    return d;
  };
  revs.forEach((r) => depth(r.id));
  return memo;
}

function LineagePanel({ id }: { id: string }) {
  const { toast, pollNow, go } = useApp();
  const l = useApi(() => api.lineage(id), [id]);
  const [editTitle, setEditTitle] = useState<string | null>(null);
  const [tagging, setTagging] = useState<TagTarget | null>(null);
  const [adding, setAdding] = useState(false);
  const [editParents, setEditParents] = useState<RevisionView | null>(null);
  const [showRemoved, setShowRemoved] = useState(false);
  if (!l.data) return l.error ? <ErrorBox error={l.error} /> : <Loading what="Lineage" />;
  const d = l.data;
  const revs = d.revisions.filter((r) => showRemoved || !r.removed);
  const depth = depthOf(d.revisions);
  const label = (rid: string) => {
    const r = d.revisions.find((x) => x.id === rid);
    return r?.label ?? shortId(rid);
  };
  const act = async (f: () => Promise<unknown>, done?: string) => {
    try {
      await f();
      if (done) toast(done);
      pollNow();
      l.reload();
    } catch (e) {
      toast((e as Error).message, "error");
    }
  };
  return (
    <div className="detail">
      {editTitle == null ? (
        <h3>
          {d.title}{" "}
          <button className="icon" title="Rename" onClick={() => setEditTitle(d.title)}>
            ✎
          </button>
        </h3>
      ) : (
        <form
          className="inline-form"
          onSubmit={(e) => {
            e.preventDefault();
            act(() => api.renameLineage(id, editTitle)).then(() => setEditTitle(null));
          }}
        >
          <input className="wide" autoFocus value={editTitle} onChange={(e) => setEditTitle(e.target.value)} />
          <button type="submit">Save</button>
          <button type="button" onClick={() => setEditTitle(null)}>
            Cancel
          </button>
        </form>
      )}
      {d.removed && <p className="note">This Lineage was removed; it is kept in history.</p>}
      <div>
        Tags: <TagChips tags={d.tags} onRemove={(t) => act(() => api.assign("lineage", id, t.hierarchy, null))} />{" "}
        <button className="link" onClick={() => setTagging({ kind: "lineage", id, label: d.title })}>
          tag…
        </button>
      </div>
      <div className="toolbar">
        <button className="primary" onClick={() => setAdding(true)}>
          Add revision…
        </button>
        <label>
          <input type="checkbox" checked={showRemoved} onChange={(e) => setShowRemoved(e.target.checked)} /> show removed
        </label>
        <span className="spacer" />
        <button
          onClick={() => {
            if (confirm(`Remove the Lineage “${d.title}”? It stays in history; its Blobs and their tags are unaffected.`))
              act(() => api.removeLineage(id), "Lineage removed").then(() => go({ lineage: null }));
          }}
        >
          Remove Lineage
        </button>
      </div>
      <table className="mini wide revisions">
        <thead>
          <tr>
            <th>Revision</th>
            <th>After</th>
            <th>Content</th>
            <th>Current files</th>
            <th>Tags</th>
            <th />
          </tr>
        </thead>
        <tbody>
          {revs.map((r) => (
            <tr key={r.id} className={r.removed ? "gone" : ""}>
              <td style={{ paddingLeft: 6 + 16 * (depth.get(r.id) ?? 0) }}>
                <RevisionLabel r={r} onSave={(v) => act(() => api.updateRevision(r.id, { label: v }))} />
              </td>
              <td className="small">
                {r.parents.map(label).join(", ") || <span className="muted">(root)</span>}{" "}
                {!r.removed && (
                  <button className="icon" title="Edit parents" onClick={() => setEditParents(r)}>
                    ✎
                  </button>
                )}
              </td>
              <td>
                <BlobLink id={r.blob} /> <span className="muted small">{fmtBytes(r.blob_size)} {r.format}</span>
              </td>
              <td className="small">
                {r.paths.length ? (
                  r.paths.map((p) => (
                    <div key={p}>
                      <PathLink path={p}>{baseName(p)}</PathLink>
                    </div>
                  ))
                ) : (
                  <span className="muted">no current location</span>
                )}
              </td>
              <td>
                <TagChips tags={r.tags} onRemove={(t) => act(() => api.assign("revision", r.id, t.hierarchy, null))} />{" "}
                {!r.removed && (
                  <button className="link" onClick={() => setTagging({ kind: "revision", id: r.id, label: r.label ?? shortId(r.id) })}>
                    tag…
                  </button>
                )}
              </td>
              <td>
                {!r.removed && (
                  <button className="icon" title="Remove this revision (kept in history)" onClick={() => act(() => api.removeRevision(r.id), "Revision removed")}>
                    🗑
                  </button>
                )}
              </td>
            </tr>
          ))}
          {revs.length === 0 && (
            <tr>
              <td colSpan={6} className="muted pad">
                No revisions yet.
              </td>
            </tr>
          )}
        </tbody>
      </table>
      <p className="muted small">
        Created <Ago ms={d.created_at} />. Revisions are listed parents-first; indentation shows depth in the revision graph. Revision tags apply only within this Lineage.
      </p>
      {tagging && (
        <TagDialog
          targets={[tagging]}
          onClose={() => {
            setTagging(null);
            l.reload();
          }}
        />
      )}
      {adding && (
        <AddRevisionDialog
          lineage={d}
          onClose={() => {
            setAdding(false);
            l.reload();
          }}
        />
      )}
      {editParents && (
        <ParentsDialog
          lineage={d}
          rev={editParents}
          onClose={() => setEditParents(null)}
          onSave={(ps) => act(() => api.updateRevision(editParents.id, { parents: ps })).then(() => setEditParents(null))}
        />
      )}
    </div>
  );
}

function RevisionLabel({ r, onSave }: { r: RevisionView; onSave(v: string | null): void }) {
  const [edit, setEdit] = useState<string | null>(null);
  if (edit != null)
    return (
      <form
        className="inline-form"
        onSubmit={(e) => {
          e.preventDefault();
          onSave(edit.trim() || null);
          setEdit(null);
        }}
      >
        <input autoFocus value={edit} onChange={(e) => setEdit(e.target.value)} onBlur={() => setEdit(null)} />
      </form>
    );
  return (
    <span className="rev-label" title="Click to edit the label" onClick={() => !r.removed && setEdit(r.label ?? "")}>
      {r.label ?? <span className="muted">{shortId(r.id)}</span>}
    </span>
  );
}

function leaves(l: LineageView): Set<string> {
  const live = l.revisions.filter((r) => !r.removed);
  const used = new Set(live.flatMap((r) => r.parents));
  return new Set(live.filter((r) => !used.has(r.id)).map((r) => r.id));
}

function ParentsDialog({ lineage, rev, onClose, onSave }: { lineage: LineageView; rev: RevisionView; onClose(): void; onSave(p: string[]): void }) {
  const [sel, setSel] = useState(new Set(rev.parents));
  return (
    <Modal title={`Parents of ${rev.label ?? shortId(rev.id)}`} onClose={onClose}>
      <p className="muted small">Choose the revisions this one follows. Choices that would create a cycle are rejected.</p>
      {lineage.revisions
        .filter((r) => !r.removed && r.id !== rev.id)
        .map((r) => (
          <label key={r.id} className="radio">
            <input
              type="checkbox"
              checked={sel.has(r.id)}
              onChange={() => {
                const n = new Set(sel);
                if (n.has(r.id)) n.delete(r.id);
                else n.add(r.id);
                setSel(n);
              }}
            />
            {r.label ?? shortId(r.id)}
          </label>
        ))}
      <div className="modal-foot">
        <button onClick={onClose}>Cancel</button>
        <button className="primary" onClick={() => onSave([...sel])}>
          Save
        </button>
      </div>
    </Modal>
  );
}

/** Pick verified content by searching current paths, then add it. */
function AddRevisionDialog({ lineage, onClose }: { lineage: LineageView; onClose(): void }) {
  const { toast, pollNow } = useApp();
  const [text, setText] = useState("");
  const [results, setResults] = useState<Entry[] | null>(null);
  const [pick, setPick] = useState<Entry | null>(null);
  const [label, setLabel] = useState("");
  const [parents, setParents] = useState(leaves(lineage));
  const [error, setError] = useState<string | null>(null);
  const search = async () => {
    try {
      setResults((await api.search(text)).filter((e) => e.kind === "file"));
    } catch (e) {
      setError((e as Error).message);
    }
  };
  const add = async () => {
    if (!pick?.blob) return;
    try {
      await api.addRevision(lineage.id, pick.blob, label.trim() || null, [...parents]);
      toast(`Added a revision to “${lineage.title}”`);
      pollNow();
      onClose();
    } catch (e) {
      setError((e as Error).message);
    }
  };
  return (
    <Modal title={`Add a revision to “${lineage.title}”`} onClose={onClose} wide>
      <ErrorBox error={error} />
      <form
        className="inline-form"
        onSubmit={(e) => {
          e.preventDefault();
          search();
        }}
      >
        <input className="wide" autoFocus placeholder="Search current files by path…" value={text} onChange={(e) => setText(e.target.value)} />
        <button type="submit">Search</button>
      </form>
      {results && (
        <div className="pick-list">
          {results.length === 0 && <div className="muted pad">No matching files.</div>}
          {results.map((e) => (
            <div
              key={e.path}
              className={[pick?.path === e.path ? "sel" : "", e.status === "verified" ? "" : "disabled"].join(" ")}
              title={e.status === "verified" ? "" : "Only verified files have known content: Verify first"}
              onClick={() => {
                if (e.status !== "verified") return;
                setPick(e);
                setLabel(baseName(e.path).replace(/\.[A-Za-z0-9]{1,5}$/, ""));
              }}
            >
              <span className={`badge st-${e.status}`}>{e.status}</span> {displayPath(e.path)}
            </div>
          ))}
        </div>
      )}
      {pick && (
        <>
          <div className="inline-form">
            <span>Label</span>
            <input className="wide" value={label} onChange={(e) => setLabel(e.target.value)} />
          </div>
          <div className="indent">
            <b>After</b>{" "}
            {lineage.revisions
              .filter((r) => !r.removed)
              .map((r) => (
                <label key={r.id} className="radio inline">
                  <input
                    type="checkbox"
                    checked={parents.has(r.id)}
                    onChange={() => {
                      const n = new Set(parents);
                      if (n.has(r.id)) n.delete(r.id);
                      else n.add(r.id);
                      setParents(n);
                    }}
                  />
                  {r.label ?? shortId(r.id)}
                </label>
              ))}
          </div>
          <p className="small">
            Content: <BlobLink id={pick.blob} /> verified <Ago ms={pick.verified_at} />
          </p>
        </>
      )}
      <div className="modal-foot">
        <button onClick={onClose}>Cancel</button>
        <button className="primary" disabled={!pick} onClick={add}>
          Add revision
        </button>
      </div>
    </Modal>
  );
}
