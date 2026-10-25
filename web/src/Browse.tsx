import { useMemo, useState } from "react";
import { api } from "./api";
import { Detail } from "./Detail";
import { LineageDialog, TagDialog } from "./dialogs";
import { useApi, useApp } from "./state";
import type { Counts, Entry, Operation, Status, Target, TreeNode } from "./types";
import { STATUS_LABEL, exact, explain, fmtBytes, naturalCompare, plural } from "./util";
import { Ago, Badge, BlobLink, ErrorBox, Loading } from "./widgets";

export function Browse() {
  const { nav } = useApp();
  return (
    <div className="browse">
      <aside className="tree-pane">
        <TreeItem node={{ name: "/", path: "", status: "listed", has_subdirs: true, counts: null }} depth={0} />
      </aside>
      <DirPane key={nav.path} />
      <aside className="detail-pane">
        {nav.sel != null ? <Detail path={nav.sel} /> : <div className="muted pad">Select an entry to see its evidence.</div>}
      </aside>
    </div>
  );
}

function attention(c: Counts | null): number {
  if (!c) return 0;
  return c.unidentified + c.changed + c.modified + c.issues + c.dirs_unlisted + c.dirs_stale + c.dirs_issue;
}

function TreeItem({ node, depth }: { node: Omit<TreeNode, "counts"> & { counts: Counts | null }; depth: number }) {
  const { nav, go } = useApp();
  const inside = node.path === "" || nav.path === node.path || nav.path.startsWith(`${node.path}/`);
  const [open, setOpen] = useState(inside);
  const children = useApi(() => (open ? api.tree(node.path) : Promise.resolve([] as TreeNode[])), [open, node.path]);
  const active = nav.path === node.path;
  const n = attention(node.counts);
  return (
    <div>
      <div
        className={active ? "tree-row active" : "tree-row"}
        style={{ paddingLeft: 4 + depth * 14 }}
        onClick={() => {
          go({ tab: "browse", path: node.path, sel: null });
          setOpen(true);
        }}
      >
        <span
          className="twisty"
          onClick={(e) => {
            e.stopPropagation();
            setOpen(!open);
          }}
        >
          {node.has_subdirs ? (open ? "▾" : "▸") : ""}
        </span>
        <span className="tree-name">{node.name}</span>
        {node.status !== "listed" && node.path !== "" && <span className={`dot st-${node.status}`} title={STATUS_LABEL[node.status]} />}
        {n > 0 && (
          <span className="tree-count" title="Entries below needing attention (unidentified, changed, modified, failed, or unlisted)">
            {n}
          </span>
        )}
      </div>
      {open &&
        children.data?.map((c) => <TreeItem key={c.path} node={c} depth={depth + 1} />)}
    </div>
  );
}

type SortKey = "name" | "status" | "size" | "mtime" | "verified" | "check";

const FILTERS: Status[] = ["verified", "plausible", "modified", "changed", "unidentified", "unlisted", "stale", "gone"];

function DirPane() {
  const { nav, go, runScan } = useApp();
  const [showHistory, setShowHistory] = useState(false);
  const view = useApi(() => api.dir(nav.path, showHistory), [nav.path, showHistory]);
  const [filter, setFilter] = useState("");
  const [only, setOnly] = useState<Set<string>>(new Set());
  const [sort, setSort] = useState<{ key: SortKey; desc: boolean }>({ key: "name", desc: false });
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [anchor, setAnchor] = useState<number | null>(null);
  const [scope, setScope] = useState<"tree" | "dir">("tree");
  const [dialog, setDialog] = useState<null | "tag" | "lineage">(null);

  const all = useMemo(() => [...(view.data?.entries ?? []), ...(view.data?.history ?? [])], [view.data]);
  const counts = useMemo(() => {
    const c: Record<string, number> = {};
    for (const e of all) {
      c[e.status] = (c[e.status] ?? 0) + 1;
      if (e.issue) c.issue = (c.issue ?? 0) + 1;
    }
    return c;
  }, [all]);
  const rows = useMemo(() => {
    const f = filter.toLowerCase();
    const r = all.filter((e) => (!f || e.name.toLowerCase().includes(f)) && (only.size === 0 || only.has(e.status) || (only.has("issue") && !!e.issue)));
    const val = (e: Entry): string | number => {
      switch (sort.key) {
        case "name":
          return e.name;
        case "status":
          return e.status;
        case "size":
          return e.kind === "dir" ? (e.subtree?.bytes ?? 0) : (e.size ?? -1);
        case "mtime":
          return e.mtime_ms ?? 0;
        case "verified":
          return e.verified_at ?? 0;
        case "check":
          return e.check?.at ?? 0;
      }
    };
    r.sort((a, b) => {
      const dirFirst = Number(b.kind === "dir") - Number(a.kind === "dir");
      if (sort.key === "name" && dirFirst) return dirFirst;
      const va = val(a);
      const vb = val(b);
      const c = typeof va === "number" && typeof vb === "number" ? va - vb : naturalCompare(String(va), String(vb));
      return sort.desc ? -c : c;
    });
    return r;
  }, [all, filter, only, sort]);

  const selEntries = rows.filter((e) => selected.has(e.path));
  const toggle = (i: number, shift: boolean) => {
    const next = new Set(selected);
    if (shift && anchor != null) {
      const [a, b] = anchor < i ? [anchor, i] : [i, anchor];
      const on = !selected.has(rows[i].path);
      for (let k = a; k <= b; k++) {
        if (on) next.add(rows[k].path);
        else next.delete(rows[k].path);
      }
    } else if (next.has(rows[i].path)) next.delete(rows[i].path);
    else next.add(rows[i].path);
    setAnchor(i);
    setSelected(next);
  };

  if (!view.data) return <section className="dir-pane">{view.error ? <ErrorBox error={view.error} /> : <Loading what="directory" />}</section>;
  const d = view.data;
  const dir = d.dir ?? d.previous;

  const targetsFor = (entries: Entry[], op: Operation): Target[] =>
    entries
      .filter((e) => e.present)
      .map((e) => (e.kind === "dir" ? { path: e.path, kind: "dir" as const, depth: op === "inventory" ? 0 : null } : { path: e.path, kind: "file" as const }));
  const scanDir = (op: Operation) => runScan(op, [{ path: nav.path, kind: "dir", depth: scope === "dir" ? 0 : null }]);
  const sortBy = (key: SortKey) => setSort((s) => ({ key, desc: s.key === key ? !s.desc : false }));
  const th = (key: SortKey, label: string, cls = "") => (
    <th className={`sortable ${cls}`} onClick={() => sortBy(key)}>
      {label}
      {sort.key === key ? (sort.desc ? " ▼" : " ▲") : ""}
    </th>
  );
  const allExact = selEntries.length > 0 && selEntries.every(exact);
  const st = dir?.subtree;

  return (
    <section className="dir-pane">
      <div className="crumbs">
        {d.crumbs.map((c, i) => (
          <span key={c.path}>
            {i > 0 && <span className="sep">/</span>}
            <a
              href="#"
              onClick={(e) => {
                e.preventDefault();
                go({ path: c.path, sel: null });
              }}
            >
              {i === 0 ? "root" : c.name}
            </a>
          </span>
        ))}
        {dir && <Badge status={dir.status} issue={!!dir.listing?.issue} />}
      </div>
      {dir ? (
        <div className="dir-summary">
          <p>{explain(dir)}</p>
          {st && (
            <p className="muted">
              Below: {plural(st.files, "file")} ({fmtBytes(st.bytes)}) — {st.verified} verified, {st.plausible} plausible, {st.modified} modified, {st.changed} changed,{" "}
              {st.unidentified} unidentified{st.issues ? `, ${st.issues} with failed reads` : ""}; {plural(st.dirs, "directory", "directories")}
              {st.dirs_unlisted ? `, ${st.dirs_unlisted} never listed` : ""}
              {st.dirs_stale ? `, ${st.dirs_stale} stale` : ""}.
            </p>
          )}
        </div>
      ) : (
        <div className="dir-summary">
          <p>Nothing is known about this location yet. Inventory it to observe its entries.</p>
        </div>
      )}
      <div className="toolbar">
        <span className="group">
          <button onClick={() => scanDir("inventory")} title="List entries and metadata; reads no file content">
            Inventory
          </button>
          <button onClick={() => scanDir("verify")} title="Full read of every file: exact content identity">
            Verify
          </button>
          <button onClick={() => scanDir("check")} title="Sampled comparison against known content (full read for small files)">
            Check
          </button>
          <select value={scope} onChange={(e) => setScope(e.target.value as "tree" | "dir")} title="Scope of the directory actions">
            <option value="tree">this directory and below</option>
            <option value="dir">this directory only</option>
          </select>
        </span>
        <span className="group">
          <input placeholder="Filter names…" value={filter} onChange={(e) => setFilter(e.target.value)} />
          <label title="Also show entries that are no longer present at their path">
            <input type="checkbox" checked={showHistory} onChange={(e) => setShowHistory(e.target.checked)} /> history
          </label>
        </span>
      </div>
      <div className="chips">
        {[...FILTERS, "issue"].map((s) =>
          counts[s] ? (
            <button
              key={s}
              className={only.has(s) ? `chip on st-${s}` : `chip st-${s}`}
              onClick={() => {
                const n = new Set(only);
                if (n.has(s)) n.delete(s);
                else n.add(s);
                setOnly(n);
              }}
            >
              {s === "issue" ? "failed reads" : STATUS_LABEL[s as Status]} {counts[s]}
            </button>
          ) : null,
        )}
        {only.size > 0 && (
          <button className="chip" onClick={() => setOnly(new Set())}>
            show all
          </button>
        )}
      </div>
      {selEntries.length > 0 && (
        <div className="selbar">
          <b>{plural(selEntries.length, "selected entry", "selected entries")}</b>
          <button onClick={() => runScan("inventory", targetsFor(selEntries, "inventory"))}>Inventory</button>
          <button onClick={() => runScan("verify", targetsFor(selEntries, "verify"))}>Verify</button>
          <button onClick={() => runScan("check", targetsFor(selEntries, "check"))}>Check</button>
          <button disabled={!allExact} title={allExact ? "Tag the selected files' content" : "Only verified files have known content: Verify first"} onClick={() => setDialog("tag")}>
            Tag…
          </button>
          <button disabled={!allExact} title={allExact ? "Record the selected files' content as revisions" : "Only verified files have known content: Verify first"} onClick={() => setDialog("lineage")}>
            Add to Lineage…
          </button>
          <button className="link" onClick={() => setSelected(new Set())}>
            clear
          </button>
        </div>
      )}
      <div className="table-wrap">
        <table className="entries fixed">
          <colgroup>
            <col style={{ width: 28 }} />
            <col />
            <col style={{ width: 100 }} />
            <col style={{ width: 70 }} />
            <col style={{ width: 76 }} />
            <col style={{ width: 196 }} />
            <col style={{ width: 76 }} />
            <col style={{ width: 92 }} />
          </colgroup>
          <thead>
            <tr>
              <th className="cb">
                <input
                  type="checkbox"
                  checked={rows.length > 0 && rows.every((r) => selected.has(r.path))}
                  onChange={(e) => setSelected(e.target.checked ? new Set(rows.map((r) => r.path)) : new Set())}
                />
              </th>
              {th("name", "Name")}
              {th("status", "Status")}
              {th("size", "Size", "num")}
              {th("mtime", "Modified")}
              <th>Content</th>
              {th("verified", "Verified")}
              {th("check", "Last check")}
            </tr>
          </thead>
          <tbody>
            {rows.map((e, i) => (
              <tr
                key={`${e.path}:${e.epoch}:${e.first_seen_at}`}
                className={[nav.sel === e.path ? "sel" : "", e.present ? "" : "gone", selected.has(e.path) ? "checked" : ""].join(" ")}
                onClick={() => go({ sel: e.path })}
                onDoubleClick={() => e.kind === "dir" && e.present && go({ path: e.path, sel: null })}
              >
                <td className="cb" onClick={(ev) => ev.stopPropagation()}>
                  <input type="checkbox" checked={selected.has(e.path)} onChange={() => {}} onClick={(ev) => toggle(i, ev.shiftKey)} />
                </td>
                <td className="name" title={e.name}>
                  <span className="icon-kind">{e.kind === "dir" ? "📁" : e.kind === "symlink" ? "↪" : e.kind === "file" ? "" : "◇"}</span>
                  {e.name}
                </td>
                <td>
                  <Badge status={e.status} issue={!!e.issue || !!e.listing?.issue} />
                </td>
                <td className="num">{e.kind === "dir" ? (e.subtree ? <span className="muted">{plural(e.subtree.files, "file")}</span> : "") : fmtBytes(e.size)}</td>
                <td>
                  <Ago ms={e.mtime_ms} />
                </td>
                <td className="clip">
                  {e.tags.length > 0 && (
                    <span className={e.status === "verified" ? "tag-mark" : "tag-mark dim"} title={e.tags.map((t) => `${t.hierarchy_name}: ${t.tag_path}`).join("\n")}>
                      ●{e.tags.length > 1 ? e.tags.length : ""}
                    </span>
                  )}
                  {e.lineages > 0 && <span className="lin-mark" title="This content is a revision in a Lineage">◆</span>}
                  {e.blob && <BlobLink id={e.blob} />} <span className="muted">{e.format ?? ""}</span>
                </td>
                <td>{e.verified_at ? <Ago ms={e.verified_at} /> : ""}</td>
                <td>{e.check ? <span className={`check ck-${e.check.result}`}>{e.check.result.replace("_", " ")}</span> : ""}</td>
              </tr>
            ))}
            {rows.length === 0 && (
              <tr>
                <td colSpan={8} className="muted pad">
                  {all.length ? "No entries match the filter." : dir?.status === "unlisted" ? "Not listed yet — its entries are unknown." : "No entries."}
                </td>
              </tr>
            )}
          </tbody>
        </table>
        {d.truncated && <div className="muted pad">Only the first 20 000 entries are shown.</div>}
      </div>
      {dialog === "tag" && (
        <TagDialog targets={selEntries.map((e) => ({ kind: "blob", id: e.blob!, label: e.name }))} onClose={() => setDialog(null)} />
      )}
      {dialog === "lineage" && (
        <LineageDialog items={selEntries.map((e) => ({ blob: e.blob!, path: e.path, name: e.name }))} onClose={() => setDialog(null)} />
      )}
    </section>
  );
}
