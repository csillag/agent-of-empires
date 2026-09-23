import type { SessionDirInput } from "../../../lib/types";

interface Props {
  dirs: SessionDirInput[];
  onChange: (dirs: SessionDirInput[]) => void;
}

/** The session's directory list: directories the agent may reach besides its
 *  working directory, each read-only or read-write. Pre-filled from
 *  `[session] default_dirs`; every row can be edited or removed. Agents that
 *  sandbox themselves enforce it, see `src/session/session_dirs.rs`. */
export function SessionDirsEditor({ dirs, onChange }: Props) {
  const update = (index: number, patch: Partial<SessionDirInput>) =>
    onChange(dirs.map((d, i) => (i === index ? { ...d, ...patch } : d)));
  const remove = (index: number) => onChange(dirs.filter((_, i) => i !== index));
  const add = () => onChange([...dirs, { path: "", access: "read-only" }]);

  return (
    <div>
      <h2 className="text-lg font-semibold text-text-primary mb-1">Directories</h2>
      <p className="text-sm text-text-muted mb-3">
        What the agent may reach besides its working directory. Everything else stays out of reach for a sandboxed
        agent.
      </p>
      {dirs.length === 0 && <p className="text-xs text-text-dim mb-2">Only the working directory.</p>}
      <ul className="space-y-2 mb-2" aria-label="Session directories">
        {dirs.map((d, i) => (
          <li key={i} className="flex items-center gap-2">
            <input
              type="text"
              value={d.path}
              onChange={(e) => update(i, { path: e.target.value })}
              placeholder="/absolute/path"
              aria-label={`Directory ${i + 1} path`}
              className="flex-1 min-w-0 bg-surface-900 border border-surface-700 rounded-lg px-3 py-2 text-sm font-mono text-text-primary placeholder:text-text-dim focus:border-brand-600 focus:outline-none"
            />
            <select
              value={d.access}
              onChange={(e) => update(i, { access: e.target.value as SessionDirInput["access"] })}
              aria-label={`Directory ${i + 1} access`}
              className="bg-surface-900 border border-surface-700 rounded-lg px-2 py-2 text-sm text-text-primary focus:border-brand-600 focus:outline-none"
            >
              <option value="read-only">read-only</option>
              <option value="read-write">read-write</option>
            </select>
            <button
              type="button"
              onClick={() => remove(i)}
              aria-label={`Remove directory ${i + 1}`}
              className="px-2 py-2 text-sm text-text-dim hover:text-text-primary cursor-pointer"
            >
              ✕
            </button>
          </li>
        ))}
      </ul>
      <button
        type="button"
        onClick={add}
        className="text-sm text-text-secondary hover:text-text-primary cursor-pointer"
      >
        + Add directory
      </button>
    </div>
  );
}
