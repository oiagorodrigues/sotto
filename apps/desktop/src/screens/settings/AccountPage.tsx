import { Icon } from "../../components/Icon";
import type { AccountView } from "../../generated/AccountView";
import { dispatch } from "../../lib/snapshot";
import "./account.css";

const STATUS: Record<AccountView["state"], string> = {
  signed_out: "Signed out",
  waiting_for_browser: "Waiting for the browser",
  signed_in: "Signed in",
  error: "Not signed in",
};

function lastSynced(ms: number | null): string {
  if (ms === null) return "Not yet";
  const minutes = Math.floor((Date.now() - ms) / 60_000);
  if (minutes < 1) return "Just now";
  if (minutes < 60) return `${minutes} min ago`;
  return new Date(ms).toLocaleString([], { dateStyle: "medium", timeStyle: "short" });
}

export function AccountPage({ account }: { account: AccountView }) {
  const signedIn = account.state === "signed_in";
  const waiting = account.state === "waiting_for_browser";

  return (
    <>
      <h1>Account</h1>
      <p className="sub">Sign in to keep your settings in sync across Macs.</p>
      <section className="mod acct" aria-labelledby="acct-name">
        <div className="mod-h">
          <h3 id="acct-name">Sotto account</h3>
          <span className="status">
            <span className={signedIn ? "led on" : "led"} aria-hidden="true" />
            {STATUS[account.state]}
          </span>
        </div>
        <div className="mod-b">
          {account.error ? (
            <p className="field-error" role="alert">
              <Icon name="alert" size={12} />
              {account.error}
            </p>
          ) : null}
          {signedIn ? (
            <>
              <div className="field">
                <span className="lbl">Email</span>
                <div className="keybox">
                  <Icon name="user" />
                  <span className="mono">{account.email ?? "…"}</span>
                </div>
              </div>
              <div className="field">
                <span className="lbl">Last synced</span>
                <span className="acct-synced">{lastSynced(account.last_synced_ms)}</span>
              </div>
              {account.sync_error ? (
                <p className="field-error" role="alert">
                  <Icon name="alert" size={12} />
                  {account.sync_error}
                </p>
              ) : null}
              <div className="row">
                <button
                  type="button"
                  className="btn sm"
                  disabled={account.syncing}
                  onClick={() => void dispatch({ type: "sync_now" })}
                >
                  <Icon name="refresh" size={14} />
                  {account.syncing ? "Syncing" : "Sync now"}
                </button>
                <button
                  type="button"
                  className="btn sm"
                  onClick={() => void dispatch({ type: "sign_out" })}
                >
                  Sign out
                </button>
              </div>
            </>
          ) : (
            <div className="row">
              <button
                type="button"
                className="btn primary sm"
                onClick={() => void dispatch({ type: "sign_in" })}
              >
                {waiting ? "Open the browser again" : "Sign in"}
              </button>
              {waiting ? (
                <button
                  type="button"
                  className="btn sm"
                  onClick={() => void dispatch({ type: "sign_out" })}
                >
                  Cancel
                </button>
              ) : null}
            </div>
          )}
        </div>
      </section>
    </>
  );
}
