import { useEffect, useMemo, useState } from "react";
import { ApiError, api } from "./api";
import { useApi, useApp } from "./state";
import type { LineageView, ProposedRevision, Suggestion } from "./types";
import { baseName, displayPath, naturalCompare, plural, shortId } from "./util";
import { BlobLink, ErrorBox, Modal, PathLink } from "./widgets";

const stem = (name: string) => name.replace(/\.[A-Za-z0-9]{1,5}$/, "");

export interface TagTarget {
  kind: "blob" | "lineage" | "revision";
  id: string;
  label: string;
}

export function TagDialog({ targets, onClose }: { targets: TagTarget[]; onClose(): void }) {
  const { toast, pollNow } = useApp();
  const tags = useApi(() => api.tags(), []);
  const [hier, setHier] = useState<string | null>(null);
  const [tag, setTag] = useState<string | null | undefined>(undefined);
  const [newHier, setNewHier] = useState("");
  const [newTag, setNewTag] = useState("");
  const [parent, setParent] = useState<string>("");
  const [error, setError] = useState<string | null>(null);
  const uniq = useMemo(() => [...new Map(targets.map((t) => [t.id, t])).values()], [targets]);
  const h = tags.data?.find((x) => x.id === hier);
  useEffect(() => {
    if (!hier && tags.data?.length) setHier(tags.data[0].id);
  }, [tags.data, hier]);

  const createHier = async () => {
    try {
      const r = await api.createHierarchy(newHier);
      setNewHier("");
      tags.reload();
      setHier(r.id);
      setTag(undefined);
    } catch (e) {
      setError((e as Error).message);
    }
  };
  const createTag = async () => {
    if (!hier) return;
    try {
      const r = await api.createTag(hier, parent || null, newTag);
      setNewTag("");
      tags.reload();
      setTag(r.id);
    } catch (e) {
      setError((e as Error).message);
    }
  };
  const apply = async () => {
    if (!hier || tag === undefined) return;
    try {
      for (const t of uniq) await api.assign(t.kind, t.id, hier, tag);
      toast(tag ? `Tagged ${plural(uniq.length, t0(uniq))}` : `Cleared the ${h?.name} tag of ${plural(uniq.length, t0(uniq))}`);
      pollNow();
      onClose();
    } catch (e) {
      setError((e as Error).message);
    }
  };

  return (
    <Modal title={`Tag ${uniq.length === 1 ? uniq[0].label : plural(uniq.length, t0(uniq))}`} onClose={onClose} wide>
      <p className="muted small">
        Tags attach to {uniq[0]?.kind === "blob" ? "content (Blobs): every copy of identical bytes shares them" : `the ${uniq[0]?.kind}`}. Each hierarchy is exclusive: a target holds at most one tag
        from it, and choosing another replaces it.
      </p>
      <ErrorBox error={error ?? tags.error} />
      <div className="two-col">
        <div>
          <b>Hierarchy</b>
          {tags.data?.map((x) => (
            <label key={x.id} className="radio">
              <input
                type="radio"
                checked={hier === x.id}
                onChange={() => {
                  setHier(x.id);
                  setTag(undefined);
                }}
              />
              {x.name}
            </label>
          ))}
          {tags.data?.length === 0 && <div className="muted">No hierarchies yet.</div>}
          <div className="inline-form">
            <input placeholder="New hierarchy, e.g. Topic" value={newHier} onChange={(e) => setNewHier(e.target.value)} />
            <button disabled={!newHier.trim()} onClick={createHier}>
              Create
            </button>
          </div>
        </div>
        <div>
          <b>Tag</b>
          {h && (
            <>
              <label className="radio">
                <input type="radio" checked={tag === null} onChange={() => setTag(null)} />
                <i>none (clear this hierarchy)</i>
              </label>
              {h.tags.map((t) => (
                <label key={t.id} className="radio" style={{ paddingLeft: 6 + 14 * (t.path.split("/").length - 1) }}>
                  <input type="radio" checked={tag === t.id} onChange={() => setTag(t.id)} />
                  {t.name} <span className="muted small">{t.usage || ""}</span>
                </label>
              ))}
              <div className="inline-form">
                <input placeholder="New tag" value={newTag} onChange={(e) => setNewTag(e.target.value)} />
                <select value={parent} onChange={(e) => setParent(e.target.value)} title="Parent tag">
                  <option value="">at top level</option>
                  {h.tags.map((t) => (
                    <option key={t.id} value={t.id}>
                      under {t.path}
                    </option>
                  ))}
                </select>
                <button disabled={!newTag.trim()} onClick={createTag}>
                  Create
                </button>
              </div>
            </>
          )}
        </div>
      </div>
      <div className="modal-foot">
        <button onClick={onClose}>Cancel</button>
        <button className="primary" disabled={!hier || tag === undefined} onClick={apply}>
          Apply
        </button>
      </div>
    </Modal>
  );
}

function t0(t: TagTarget[]): string {
  return t[0]?.kind === "blob" ? "file content" : (t[0]?.kind ?? "item");
}

interface Item {
  blob: string;
  path: string;
  name: string;
}

/** Record verified content as revisions: a new Lineage or an existing one. */
export function LineageDialog({ items, onClose }: { items: Item[]; onClose(): void }) {
  const { toast, pollNow, go } = useApp();
  const initial = useMemo(() => {
    const seen = new Set<string>();
    return [...items].sort((a, b) => naturalCompare(a.name, b.name)).filter((i) => !seen.has(i.blob) && seen.add(i.blob));
  }, [items]);
  const [rows, setRows] = useState(initial.map((i) => ({ ...i, label: stem(i.name) })));
  const [mode, setMode] = useState<"new" | "existing">("new");
  const [title, setTitle] = useState(stem(initial[0]?.name ?? "").replace(/\s*[([].*$/, "") || "Untitled");
  const lineages = useApi(() => api.lineages(), []);
  const [lineage, setLineage] = useState<string>("");
  const [detail, setDetail] = useState<LineageView | null>(null);
  const [parents, setParents] = useState<Set<string>>(new Set());
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!lineage) return setDetail(null);
    api.lineage(lineage).then((d) => {
      setDetail(d);
      const live = d.revisions.filter((r) => !r.removed);
      const used = new Set(live.flatMap((r) => r.parents));
      setParents(new Set(live.filter((r) => !used.has(r.id)).map((r) => r.id)));
    }, (e) => setError(e.message));
  }, [lineage]);

  const move = (i: number, d: number) => {
    const r = [...rows];
    const [x] = r.splice(i, 1);
    r.splice(i + d, 0, x);
    setRows(r);
  };
  const submit = async () => {
    try {
      if (mode === "new") {
        const r = await api.createLineage(
          title,
          rows.map((x, i) => ({ blob: x.blob, label: x.label || null, parents: i > 0 ? [i - 1] : [] })),
        );
        toast(`Created Lineage “${title}” with ${plural(rows.length, "revision")}`);
        go({ tab: "lineages", lineage: r.id });
      } else {
        let prev: string[] = [...parents];
        for (const x of rows) {
          const r = await api.addRevision(lineage, x.blob, x.label || null, prev);
          prev = [r.id];
        }
        toast(`Added ${plural(rows.length, "revision")} to “${detail?.title}”`);
      }
      pollNow();
      onClose();
    } catch (e) {
      setError((e as Error).message);
    }
  };

  return (
    <Modal title="Add to Lineage" onClose={onClose} wide>
      <p className="muted small">
        A Lineage records that exact contents are revisions of one thing. The revisions below form a chain in this order (first is earliest). This is your decision; it changes no scan evidence.
        {items.length > initial.length && " Identical contents in the selection were merged."}
      </p>
      <ErrorBox error={error} />
      <div className="inline-form">
        <label className="radio">
          <input type="radio" checked={mode === "new"} onChange={() => setMode("new")} /> New Lineage titled
        </label>
        <input className="wide" value={title} disabled={mode !== "new"} onChange={(e) => setTitle(e.target.value)} />
      </div>
      <div className="inline-form">
        <label className="radio">
          <input type="radio" checked={mode === "existing"} onChange={() => setMode("existing")} disabled={!lineages.data?.length} /> Existing Lineage
        </label>
        <select value={lineage} disabled={mode !== "existing"} onChange={(e) => setLineage(e.target.value)}>
          <option value="">choose…</option>
          {lineages.data?.map((l) => (
            <option key={l.id} value={l.id}>
              {l.title} ({l.revisions})
            </option>
          ))}
        </select>
      </div>
      {mode === "existing" && detail && (
        <div className="indent">
          <b>After</b>{" "}
          {detail.revisions
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
          {detail.revisions.length === 0 && <span className="muted">(no revisions yet)</span>}
        </div>
      )}
      <table className="mini wide">
        <thead>
          <tr>
            <th />
            <th>Label</th>
            <th>Content</th>
            <th>From</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((r, i) => (
            <tr key={r.blob}>
              <td className="nowrap">
                <button className="icon" disabled={i === 0} onClick={() => move(i, -1)} title="Earlier">
                  ▲
                </button>
                <button className="icon" disabled={i === rows.length - 1} onClick={() => move(i, 1)} title="Later">
                  ▼
                </button>
              </td>
              <td>
                <input className="wide" value={r.label} onChange={(e) => setRows(rows.map((x, k) => (k === i ? { ...x, label: e.target.value } : x)))} />
              </td>
              <td>
                <BlobLink id={r.blob} />
              </td>
              <td className="small">{displayPath(r.path)}</td>
            </tr>
          ))}
        </tbody>
      </table>
      <div className="modal-foot">
        <button onClick={onClose}>Cancel</button>
        <button className="primary" disabled={rows.length === 0 || (mode === "new" ? !title.trim() : !lineage)} onClick={submit}>
          {mode === "new" ? "Create Lineage" : "Add revisions"}
        </button>
      </div>
    </Modal>
  );
}

/** Review and accept an advisor's semantic proposal, with edits. */
export function ReviewDialog({ s, onClose, onDone }: { s: Suggestion; onClose(): void; onDone(): void }) {
  const { toast, pollNow } = useApp();
  const [error, setError] = useState<string | null>(null);
  const a = s.action;
  const proposed: ProposedRevision[] = a.type === "create_lineage" ? a.revisions : a.type === "add_revision" ? [a.revision] : [];
  const [title, setTitle] = useState(a.type === "create_lineage" ? a.title : "");
  const [rows, setRows] = useState(proposed.map((r) => ({ ...r, label: r.label ?? stem(baseName(r.path)), include: true })));

  const accept = async () => {
    try {
      if (a.type === "create_lineage") {
        const inc = rows.filter((r) => r.include);
        const modifications = {
          title: title !== a.title ? title : undefined,
          excluded: rows.filter((r) => !r.include).map((r) => r.blob),
          labels: Object.fromEntries(inc.filter((r, i) => r.label !== (proposed[i]?.label ?? "")).map((r) => [r.blob, r.label])),
        };
        await api.createLineage(
          title,
          inc.map((r, i) => ({ blob: r.blob, label: r.label || null, parents: i > 0 ? [i - 1] : [] })),
          s.id,
          modifications,
        );
        toast(`Created Lineage “${title}”`);
      } else if (a.type === "add_revision") {
        await api.addRevision(a.lineage, a.revision.blob, rows[0].label || null, [a.parent], s.id);
        toast(`Added a revision to “${a.lineage_title}”`);
      }
      pollNow();
      onDone();
    } catch (e) {
      if (e instanceof ApiError && e.status === 409) {
        toast(`${e.message}`, "error");
        onDone();
      } else setError((e as Error).message);
    }
  };

  return (
    <Modal title="Review proposal" onClose={onClose} wide>
      <p>
        <b>{s.title}</b>
      </p>
      <p>{s.reason}</p>
      <p className="note">
        <b>Uncertainty:</b> {s.uncertainty}
      </p>
      <ErrorBox error={error} />
      {a.type === "add_revision" && (
        <p>
          Add the content now at <PathLink path={a.revision.path} /> (<BlobLink id={a.revision.blob} />) to Lineage <b>{a.lineage_title}</b>, after revision{" "}
          <b>{a.parent_label ?? shortId(a.parent)}</b>.
        </p>
      )}
      {a.type === "create_lineage" && (
        <div className="inline-form">
          <span>New Lineage</span>
          <input className="wide" value={title} onChange={(e) => setTitle(e.target.value)} />
        </div>
      )}
      <table className="mini wide">
        <thead>
          <tr>
            {a.type === "create_lineage" && <th>Include</th>}
            <th>Label</th>
            <th>Content</th>
            <th>File</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((r, i) => (
            <tr key={r.blob} className={r.include ? "" : "gone"}>
              {a.type === "create_lineage" && (
                <td>
                  <input type="checkbox" checked={r.include} onChange={() => setRows(rows.map((x, k) => (k === i ? { ...x, include: !x.include } : x)))} />
                </td>
              )}
              <td>
                <input className="wide" value={r.label} onChange={(e) => setRows(rows.map((x, k) => (k === i ? { ...x, label: e.target.value } : x)))} />
              </td>
              <td>
                <BlobLink id={r.blob} />
              </td>
              <td className="small">
                <PathLink path={r.path} />
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {a.type === "create_lineage" && <p className="muted small">Included revisions form a chain in the order shown (first is earliest).</p>}
      <div className="modal-foot">
        <button onClick={onClose}>Cancel</button>
        <button className="primary" disabled={a.type === "create_lineage" && (rows.filter((r) => r.include).length === 0 || !title.trim())} onClick={accept}>
          Accept
        </button>
      </div>
    </Modal>
  );
}
