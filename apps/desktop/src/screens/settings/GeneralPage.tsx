import { dispatch } from "../../lib/snapshot";

export function GeneralPage() {
  return (
    <>
      <h1>General</h1>
      <div className="mods">
        <section className="mod" aria-labelledby="setup-name">
          <div className="mod-h">
            <h3 id="setup-name">Setup</h3>
          </div>
          <div className="mod-b">
            <p className="free">
              <b>Welcome tour</b>
              <span>Consent, permissions, transcription check and models.</span>
            </p>
            <div className="row">
              <button
                type="button"
                className="btn sm"
                onClick={() => void dispatch({ type: "reopen_onboarding" })}
              >
                Run setup again
              </button>
            </div>
          </div>
        </section>
      </div>
    </>
  );
}
