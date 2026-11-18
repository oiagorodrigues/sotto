import { useEffect, useRef, useState } from "react";
import { Dialog } from "../../components/Dialog";
import { Icon } from "../../components/Icon";
import { IconButton } from "../../components/IconButton";
import { NoticeBar } from "../../components/NoticeBar";
import type { AppSnapshot } from "../../generated/AppSnapshot";
import type { ModeFileView } from "../../generated/ModeFileView";
import type { ModeView } from "../../generated/ModeView";
import { dispatch } from "../../lib/snapshot";
import { filesSummary, MAX_NAME, MAX_PROMPT, progressText, tokensText } from "./modes";
import "./modes.css";

export function ModesPage({ snapshot }: { snapshot: AppSnapshot }) {
  const { modes, selected_mode_id } = snapshot;
  const [chosen, setChosen] = useState<string | null>(null);
  const known = useRef(new Set(modes.map((m) => m.id)));
  const creating = useRef(false);

  // A mode that appears after "New mode" is the one to edit.
  useEffect(() => {
    const fresh = modes.find((m) => !known.current.has(m.id));
    known.current = new Set(modes.map((m) => m.id));
    if (fresh && creating.current) {
      creating.current = false;
      setChosen(fresh.id);
    }
  }, [modes]);

  const current = modes.find((m) => m.id === chosen) ?? modes[0];

  const create = () => {
    creating.current = true;
    void dispatch({ type: "create_mode", name: "New mode", system_prompt: "" });
  };

  return (
    <>
      <h1>Modes</h1>
      <p className="sub">A prompt and files that shape your answers.</p>
      <NoticeBar notice={snapshot.notice} />
      {modes.length === 0 ? (
        <section className="mod modes-empty" aria-labelledby="modes-empty-title">
          <div className="mod-b">
            <h3 id="modes-empty-title">No modes yet</h3>
            <p>Create one for each kind of call, then pick it in the overlay.</p>
            <div className="row">
              <button type="button" className="btn primary" onClick={create}>
                <Icon name="plus" size={14} />
                New mode
              </button>
            </div>
          </div>
        </section>
      ) : (
        <div className="modes-grid">
          <nav aria-label="Modes" className="mode-nav">
            <ul>
              {modes.map((m) => (
                <li key={m.id}>
                  <button
                    type="button"
                    aria-current={m.id === current?.id ? "true" : undefined}
                    onClick={() => setChosen(m.id)}
                  >
                    <span className="mode-name">{m.name}</span>
                    <span className="mode-meta">
                      {m.id === selected_mode_id ? "Next session · " : ""}
                      {filesSummary(m.files.length, m.total_tokens)}
                    </span>
                  </button>
                </li>
              ))}
            </ul>
            <button type="button" className="btn" onClick={create}>
              <Icon name="plus" size={14} />
              New mode
            </button>
          </nav>
          {current ? <Editor key={current.id} mode={current} /> : null}
        </div>
      )}
    </>
  );
}

function Editor({ mode }: { mode: ModeView }) {
  const [name, setName] = useState(mode.name);
  const [prompt, setPrompt] = useState(mode.system_prompt);
  const [confirmDelete, setConfirmDelete] = useState(false);

  useEffect(() => {
    setName(mode.name);
    setPrompt(mode.system_prompt);
  }, [mode.name, mode.system_prompt]);

  const trimmed = name.trim();
  const dirty = trimmed !== mode.name || prompt !== mode.system_prompt;
  const save = () => {
    if (!dirty || trimmed === "") return;
    void dispatch({
      type: "update_mode",
      mode_id: mode.id,
      name: trimmed,
      system_prompt: prompt,
    });
  };

  return (
    <div className="mods">
      <section className="mod" aria-labelledby="mode-title">
        <div className="mod-h">
          <h3 id="mode-title">Mode</h3>
        </div>
        <form
          className="mod-b"
          onSubmit={(e) => {
            e.preventDefault();
            save();
          }}
        >
          <div className="field">
            <label htmlFor="mode-name">Name</label>
            <input
              id="mode-name"
              className="input"
              value={name}
              maxLength={MAX_NAME}
              placeholder="Sales discovery"
              aria-invalid={trimmed === "" ? true : undefined}
              aria-describedby="mode-name-error"
              onChange={(e) => setName(e.target.value)}
            />
            {trimmed === "" ? (
              <p className="field-error" id="mode-name-error" role="alert">
                <Icon name="alert" size={12} />
                Give the mode a name.
              </p>
            ) : null}
          </div>
          <div className="field">
            <label htmlFor="mode-prompt">Prompt</label>
            <textarea
              id="mode-prompt"
              className="input prompt"
              value={prompt}
              maxLength={MAX_PROMPT}
              rows={7}
              placeholder="You are helping me in sales calls. Keep answers under three sentences."
              onChange={(e) => setPrompt(e.target.value)}
            />
          </div>
          <div className="row actions">
            <button type="button" className="btn danger" onClick={() => setConfirmDelete(true)}>
              <Icon name="trash" size={14} />
              Delete mode
            </button>
            <span className="grow" />
            <button
              type="button"
              className="btn"
              disabled={!dirty}
              onClick={() => {
                setName(mode.name);
                setPrompt(mode.system_prompt);
              }}
            >
              Discard
            </button>
            <button type="submit" className="btn primary" disabled={!dirty || trimmed === ""}>
              Save
            </button>
          </div>
        </form>
      </section>
      <Files mode={mode} />
      {confirmDelete && (
        <Dialog
          title={`Delete ${mode.name}?`}
          onClose={() => setConfirmDelete(false)}
          footer={
            <>
              <button
                type="button"
                className="btn"
                data-autofocus
                onClick={() => setConfirmDelete(false)}
              >
                Cancel
              </button>
              <button
                type="button"
                className="btn danger"
                onClick={() => {
                  void dispatch({ type: "delete_mode", mode_id: mode.id });
                  setConfirmDelete(false);
                }}
              >
                <Icon name="trash" size={14} />
                Delete mode
              </button>
            </>
          }
        >
          <p>The prompt and the files of this mode are erased from this Mac.</p>
        </Dialog>
      )}
    </div>
  );
}

function Files({ mode }: { mode: ModeView }) {
  const retry = mode.files.some((f) => f.status === "failed");
  return (
    <section className="mod" aria-labelledby="files-title">
      <div className="mod-h">
        <h3 id="files-title">Files</h3>
        <span className="status">{filesSummary(mode.files.length, mode.total_tokens)}</span>
      </div>
      <div className="mod-b">
        {mode.files.length === 0 ? (
          <p className="muted">Add .txt, .md or .pdf files up to 10 MB each.</p>
        ) : (
          <>
            <ul className="file-list">
              {mode.files.map((f) => (
                <FileRow key={f.id} file={f} modeId={mode.id} retrieval={mode.retrieval} />
              ))}
            </ul>
            <p className="muted">
              {mode.retrieval
                ? "Too large for the prompt: each answer uses the passages that match the question."
                : "Small enough to send in full with every answer."}
            </p>
          </>
        )}
        <div className="row">
          <button
            type="button"
            className="btn"
            onClick={() => void dispatch({ type: "pick_mode_files", mode_id: mode.id })}
          >
            <Icon name="plus" size={14} />
            Add files
          </button>
          {retry ? (
            <button
              type="button"
              className="btn"
              onClick={() => void dispatch({ type: "retry_indexing", mode_id: mode.id })}
            >
              <Icon name="refresh" size={14} />
              Retry indexing
            </button>
          ) : null}
        </div>
      </div>
    </section>
  );
}

function FileRow({
  file,
  modeId,
  retrieval,
}: {
  file: ModeFileView;
  modeId: string;
  retrieval: boolean;
}) {
  const indexing = file.status === "indexing";
  const pct = file.chunks_total > 0 ? (file.chunks_done / file.chunks_total) * 100 : 0;
  return (
    <li className="file-row" data-status={file.status}>
      <Icon name="file" />
      <div className="file-main">
        <span className="file-name" title={file.name}>
          {file.name}
        </span>
        <span className="file-meta">
          <span className="mono">{tokensText(file.tokens)}</span>
          {retrieval ? (
            <span className="file-state">
              {file.status === "ready" ? <Icon name="check" size={12} /> : null}
              {file.status === "failed" ? <Icon name="alert" size={12} /> : null}
              {file.status === "ready"
                ? "Ready"
                : file.status === "failed"
                  ? (file.error ?? "Indexing failed")
                  : progressText(file)}
            </span>
          ) : null}
        </span>
        {indexing ? (
          <div
            className="bar"
            role="progressbar"
            aria-label={`Indexing ${file.name}`}
            aria-valuemin={0}
            aria-valuemax={file.chunks_total}
            aria-valuenow={file.chunks_done}
          >
            <span style={{ width: `${pct}%` }} />
          </div>
        ) : null}
      </div>
      <IconButton
        icon="trash"
        label={`Remove ${file.name}`}
        onClick={() =>
          void dispatch({ type: "remove_mode_file", mode_id: modeId, file_id: file.id })
        }
      />
    </li>
  );
}
