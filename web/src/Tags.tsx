import { useState } from "react";
import { api } from "./api";
import { useApi, useApp } from "./state";
import type { Hierarchy, TagView } from "./types";
import { baseName, fmtBytes, shortId } from "./util";
import { BlobLink, ErrorBox, Loading, PathLink } from "./widgets";

export function Tags() {
  const { nav, go, toast, pollNow } = useApp();
  const tags = useApi(() => api.tags(), []);
  const [name, setName] = useState("");
  const create = async () => {
    try {
      await api.createHierarchy(name);
      setName("");
      tags.reload();
      pollNow();
    } catch (e) {
      toast((e as Error).message, "error");
    }
  };
  return (
    <div className="split">
      <div className="split-main narrow">
        <form
          className="inline-form"
          onSubmit={(e) => {
            e.preventDefault();
            create();
          }}
        >
          <input placeholder="New hierarchy, e.g. Topic" value={name} onChange={(e) => setName(e.target.value)} />
          <button type="submit" disabled={!name.trim()}>
            Create
          </button>
        </form>
        <p className="muted small">
          Each hierarchy is an exclusive tag tree: a Blob, Lineage, or revision selects at most one tag from it. Selecting a tag also satisfies queries for its ancestors.
        </p>
        {!tags.data ? (
          tags.error ? <ErrorBox error={tags.error} /> : <Loading what="tags" />
        ) : (
          tags.data.map((h) => <HierarchyTree key={h.id} h={h} selected={nav.tag} onSelect={(id) => go({ tag: id })} reload={tags.reload} />)
        )}
      </div>
      <aside className="split-side">{nav.tag ? <TagPanel id={nav.tag} /> : <div className="muted pad">Select a tag to see what carries it.</div>}</aside>
    </div>
  );
}

function HierarchyTree({ h, selected, onSelect, reload }: { h: Hierarchy; selected: string | null; onSelect(id: string): void; reload(): void }) {
  const { toast, pollNow } = useApp();
  const [adding, setAdding] = useState<string | "top" | null>(null);
  const [renaming, setRenaming] = useState<TagView | null>(null);
  const [text, setText] = useState("");
  const submit = async () => {
    try {
      if (renaming) await api.renameTag(renaming.id, text);
      else await api.createTag(h.id, adding === "top" ? null : adding, text);
      setAdding(null);
      setRenaming(null);
      setText("");
      reload();
      pollNow();
    } catch (e) {
      toast((e as Error).message, "error");
    }
  };
  const form = (
    <form
      className="inline-form"
      onSubmit={(e) => {
        e.preventDefault();
        submit();
      }}
    >
      <input autoFocus value={text} placeholder={renaming ? "New name" : "Tag name"} onChange={(e) => setText(e.target.value)} />
      <button type="submit" disabled={!text.trim()}>
        {renaming ? "Rename" : "Add"}
      </button>
      <button
        type="button"
        onClick={() => {
          setAdding(null);
          setRenaming(null);
        }}
      >
        Cancel
      </button>
    </form>
  );
  return (
    <div className="hierarchy">
      <div className="hier-head">
        <b>{h.name}</b>
        <button className="link" onClick={() => (setAdding("top"), setRenaming(null), setText(""))}>
          + tag
        </button>
      </div>
      {adding === "top" && form}
      {h.tags.map((t) => (
        <div key={t.id}>
          <div className={selected === t.id ? "tree-row active" : "tree-row"} style={{ paddingLeft: 8 + 16 * (t.path.split("/").length - 1) }} onClick={() => onSelect(t.id)}>
            <span className="tree-name">{t.name}</span>
            {t.usage > 0 && <span className="tree-count plain">{t.usage}</span>}
            <span className="row-actions">
              <button className="link" title="Add a child tag" onClick={(e) => (e.stopPropagation(), setAdding(t.id), setRenaming(null), setText(""))}>
                + child
              </button>
              <button className="link" title="Rename" onClick={(e) => (e.stopPropagation(), setRenaming(t), setAdding(null), setText(t.name))}>
                rename
              </button>
            </span>
          </div>
          {(adding === t.id || renaming?.id === t.id) && <div style={{ paddingLeft: 24 + 16 * (t.path.split("/").length - 1) }}>{form}</div>}
        </div>
      ))}
      {h.tags.length === 0 && adding !== "top" && <div className="muted small pad">No tags yet.</div>}
    </div>
  );
}

function TagPanel({ id }: { id: string }) {
  const { go } = useApp();
  const t = useApi(() => api.tagTargets(id), [id]);
  if (!t.data) return t.error ? <ErrorBox error={t.error} /> : <Loading what="tag" />;
  const d = t.data;
  return (
    <div className="detail">
      <h3>{d.tag.path}</h3>
      <p className="muted small">Includes everything tagged with a descendant tag.</p>
      <div className="card">
        <div className="card-head">
          <b>Content (Blobs)</b> <span className="muted">{d.blobs.length}</span>
        </div>
        <table className="mini">
          <tbody>
            {d.blobs.map((b) => (
              <tr key={b.id}>
                <td>
                  <BlobLink id={b.id} />
                </td>
                <td className="small muted">{fmtBytes(b.size)}</td>
                <td className="small">
                  {b.paths.length ? b.paths.map((p) => <div key={p}><PathLink path={p}>{baseName(p)}</PathLink></div>) : <span className="muted">no current location</span>}
                </td>
                <td className="small muted">{b.tag_path !== d.tag.path ? b.tag_path : ""}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {(d.lineages.length > 0 || d.revisions.length > 0) && (
        <div className="card">
          <div className="card-head">
            <b>Lineages and revisions</b>
          </div>
          <ul className="plain">
            {d.lineages.map((l) => (
              <li key={l.id}>
                <a href="#" onClick={(e) => (e.preventDefault(), go({ tab: "lineages", lineage: l.id }))}>
                  {l.label}
                </a>{" "}
                <span className="muted">(Lineage)</span>
              </li>
            ))}
            {d.revisions.map((r) => (
              <li key={r.id}>
                <a href="#" onClick={(e) => (e.preventDefault(), go({ tab: "lineages", lineage: r.lineage }))}>
                  {r.label || shortId(r.id)}
                </a>{" "}
                <span className="muted">(revision)</span>
              </li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}
