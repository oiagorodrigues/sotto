import { useState } from "react";
import { Dialog } from "../../components/Dialog";
import { Icon } from "../../components/Icon";
import type { CalendarView } from "../../generated/CalendarView";
import { looksLikeClientId, looksLikeClientSecret } from "../../lib/calendar";
import { formatClock } from "../../lib/format";
import { dispatch } from "../../lib/snapshot";
import "./calendar.css";

type Dialogs = "client" | "remove" | "disconnect" | null;

const STATUS: Record<CalendarView["connection"], string> = {
  not_configured: "Not configured",
  disconnected: "Not connected",
  connecting: "Waiting for Google",
  connected: "Connected",
  error: "Error",
};

export function CalendarPage({ calendar }: { calendar: CalendarView }) {
  const [dialog, setDialog] = useState<Dialogs>(null);
  const configured = calendar.masked_client_id !== null;
  const connected = calendar.connection === "connected";
  const connecting = calendar.connection === "connecting";

  return (
    <>
      <h1>Calendar</h1>
      <p className="sub">
        Google Calendar meetings. Nothing leaves this Mac except Google requests.
      </p>
      <div className="mods">
        <section className="mod" aria-labelledby="gclient-name">
          <div className="mod-h">
            <h3 id="gclient-name">Google client</h3>
            <span className="status">
              <span className={configured ? "led on" : "led"} aria-hidden="true" />
              {configured ? "Saved" : "Not configured"}
            </span>
          </div>
          <div className="mod-b">
            <div className="field">
              <span className="lbl">Client ID</span>
              <div className="keybox">
                <Icon name="key" />
                <span className="mono">
                  {calendar.masked_client_id ?? "…apps.googleusercontent.com"}
                </span>
              </div>
            </div>
            <div className="field">
              <span className="lbl">Client secret</span>
              <div className="keybox">
                <Icon name="key" />
                <span className="mono">{calendar.has_client_secret ? "Saved" : "Not set"}</span>
              </div>
            </div>
            <div className="row">
              {configured ? (
                <>
                  <button type="button" className="btn sm" onClick={() => setDialog("client")}>
                    <Icon name="swap" size={14} />
                    Replace
                  </button>
                  <button type="button" className="btn sm" onClick={() => setDialog("remove")}>
                    <Icon name="trash" size={14} />
                    Remove
                  </button>
                </>
              ) : (
                <button
                  type="button"
                  className="btn primary sm"
                  onClick={() => setDialog("client")}
                >
                  Add client
                </button>
              )}
            </div>
          </div>
        </section>

        <section className="mod" aria-labelledby="gaccount-name">
          <div className="mod-h">
            <h3 id="gaccount-name">Account</h3>
            <span className="status">
              <span className={connected ? "led on" : "led"} aria-hidden="true" />
              {STATUS[calendar.connection]}
            </span>
          </div>
          <div className="mod-b">
            {calendar.error ? (
              <p className="field-error" role="alert">
                <Icon name="alert" size={12} />
                {calendar.error}
              </p>
            ) : null}
            <div className="row">
              {connected ? (
                <>
                  <button
                    type="button"
                    className="btn sm"
                    onClick={() => void dispatch({ type: "calendar_refresh" })}
                  >
                    <Icon name="refresh" size={14} />
                    Refresh
                  </button>
                  <button type="button" className="btn sm" onClick={() => setDialog("disconnect")}>
                    Disconnect
                  </button>
                </>
              ) : (
                <>
                  <button
                    type="button"
                    className="btn primary sm"
                    disabled={!configured || connecting}
                    onClick={() => void dispatch({ type: "calendar_connect" })}
                  >
                    Connect
                  </button>
                  {connecting ? (
                    <button
                      type="button"
                      className="btn sm"
                      onClick={() => void dispatch({ type: "calendar_disconnect" })}
                    >
                      Cancel
                    </button>
                  ) : null}
                </>
              )}
            </div>
          </div>
        </section>
      </div>

      <section className="mod cal-events" aria-labelledby="events-name">
        <div className="mod-h">
          <h3 id="events-name">Next 24 hours</h3>
        </div>
        {calendar.events.length > 0 ? (
          <ul className="cal-list">
            {calendar.events.map((e) => (
              <li key={e.id}>
                <time className="mono">{formatClock(e.start_ms)}</time>
                <span className="cal-title">{e.title}</span>
                {e.has_video ? (
                  <span className="cal-video" role="img" aria-label="Video call">
                    <Icon name="video" size={14} />
                  </span>
                ) : null}
              </li>
            ))}
          </ul>
        ) : (
          <p className="cal-empty">
            {connected
              ? "No meetings in the next 24 hours."
              : "Connect Google to see your meetings."}
          </p>
        )}
      </section>

      {dialog === "client" && (
        <ClientDialog replacing={configured} onClose={() => setDialog(null)} />
      )}
      {dialog === "remove" && (
        <Dialog
          title="Remove the Google client?"
          onClose={() => setDialog(null)}
          footer={
            <>
              <button type="button" className="btn" data-autofocus onClick={() => setDialog(null)}>
                Cancel
              </button>
              <button
                type="button"
                className="btn danger"
                onClick={() => {
                  void dispatch({ type: "calendar_remove_client" });
                  setDialog(null);
                }}
              >
                <Icon name="trash" size={14} />
                Remove client
              </button>
            </>
          }
        >
          <p>Sotto disconnects from Google and forgets the client ID and secret.</p>
        </Dialog>
      )}
      {dialog === "disconnect" && (
        <Dialog
          title="Disconnect Google Calendar?"
          onClose={() => setDialog(null)}
          footer={
            <>
              <button type="button" className="btn" data-autofocus onClick={() => setDialog(null)}>
                Cancel
              </button>
              <button
                type="button"
                className="btn danger"
                onClick={() => {
                  void dispatch({ type: "calendar_disconnect" });
                  setDialog(null);
                }}
              >
                Disconnect
              </button>
            </>
          }
        >
          <p>Sotto revokes its access at Google. Meeting prompts stop until you connect again.</p>
        </Dialog>
      )}
    </>
  );
}

function ClientDialog({ replacing, onClose }: { replacing: boolean; onClose: () => void }) {
  const [id, setId] = useState("");
  const [secret, setSecret] = useState("");
  const [touched, setTouched] = useState(false);
  const [rejected, setRejected] = useState(false);
  const idOk = looksLikeClientId(id.trim());
  const secretOk = secret === "" || looksLikeClientSecret(secret.trim());
  const submit = () => {
    setTouched(true);
    if (!idOk || !secretOk) return;
    const client_id = id.trim();
    const client_secret = secret.trim() === "" ? null : secret.trim();
    setSecret("");
    dispatch({ type: "calendar_set_client", client_id, client_secret }).then(onClose, () =>
      setRejected(true),
    );
  };
  return (
    <Dialog
      title={replacing ? "Replace the Google client" : "Add a Google client"}
      onClose={onClose}
      footer={
        <>
          <button type="button" className="btn" onClick={onClose}>
            Cancel
          </button>
          <button type="button" className="btn primary" onClick={submit}>
            Save client
          </button>
        </>
      }
    >
      <form
        className="cal-form"
        onSubmit={(e) => {
          e.preventDefault();
          submit();
        }}
      >
        <div className="field">
          <label htmlFor="cal-client-id">Client ID</label>
          <input
            id="cal-client-id"
            data-autofocus
            className="input mono"
            autoComplete="off"
            spellCheck={false}
            placeholder="123456-abc.apps.googleusercontent.com"
            value={id}
            aria-invalid={(touched && !idOk) || rejected ? true : undefined}
            onChange={(e) => {
              setRejected(false);
              setId(e.target.value);
            }}
          />
        </div>
        <div className="field">
          <label htmlFor="cal-client-secret">Client secret (optional)</label>
          <input
            id="cal-client-secret"
            className="input mono"
            type="password"
            autoComplete="off"
            spellCheck={false}
            placeholder="GOCSPX-…"
            value={secret}
            aria-invalid={touched && !secretOk ? true : undefined}
            onChange={(e) => setSecret(e.target.value)}
          />
        </div>
        {(touched && (!idOk || !secretOk)) || rejected ? (
          <p className="field-error" role="alert">
            <Icon name="alert" size={12} />
            {rejected
              ? "Sotto could not save that client."
              : !idOk
                ? "The client ID ends with .apps.googleusercontent.com."
                : "The secret looks too short."}
          </p>
        ) : null}
      </form>
    </Dialog>
  );
}
