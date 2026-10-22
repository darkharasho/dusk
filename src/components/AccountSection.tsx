import { useEffect, useState } from "react";
import {
  cancelSignIn,
  getAccount,
  onAccount,
  openSignInPage,
  signInWithGithub,
  signOutOfGithub,
} from "../api";
import type { AccountView, SignInCode } from "../types";
import { describe } from "../errors";

/**
 * Signing in with GitHub, so your own machines pair without asking.
 *
 * This is how your machines trust each other: one signed in to the same
 * account pairs on its own, and by default nothing else can pair at all.
 */
export function AccountSection() {
  const [account, setAccount] = useState<AccountView | null>(null);
  const [code, setCode] = useState<SignInCode | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function refresh() {
    setAccount(await getAccount());
  }

  useEffect(() => {
    refresh().catch((e) => setError(describe(e)));
    let stop: (() => void) | undefined;
    let cancelled = false;
    onAccount(({ error }) => {
      setCode(null);
      setBusy(false);
      setError(error);
      refresh().catch((e) => setError(describe(e)));
    }).then((fn) => {
      if (cancelled) fn();
      else stop = fn;
    });
    return () => {
      cancelled = true;
      stop?.();
    };
  }, []);

  async function start() {
    setBusy(true);
    setError(null);
    try {
      const next = await signInWithGithub();
      setCode(next);
      await openSignInPage().catch(() => {});
    } catch (err) {
      setError(describe(err));
      setBusy(false);
    }
  }

  async function stop() {
    await cancelSignIn().catch(() => {});
    setCode(null);
    setBusy(false);
  }

  async function signOut() {
    setBusy(true);
    setError(null);
    try {
      await signOutOfGithub();
      await refresh();
    } catch (err) {
      setError(describe(err));
    } finally {
      setBusy(false);
    }
  }

  const user = account?.user ?? null;

  return (
    <div className="axi-panel axi-stack">
      {!account && !error && <p className="axi-ink-dim">Checking.</p>}

      {account && user && (
        <>
          <div className="axi-row">
            {user.avatarUrl && (
              <img className="dusk-avatar" src={user.avatarUrl} alt="" />
            )}
            <div>
              <strong>{user.login}</strong>
              <p className="axi-ink-dim">Signed in with GitHub</p>
            </div>
            <button
              type="button"
              className="axi-btn dusk-row__end"
              disabled={busy}
              onClick={signOut}
            >
              {busy ? "Signing out" : "Sign out"}
            </button>
          </div>
          <p className="axi-ink-dim">
            Machines signed in to this account pair with each other on their
            own. Nothing else can pair with this one unless Let other devices
            pair is on in Settings › Hosting.
          </p>
          {account.machines.length > 0 && (
            <div className="axi-stack" style={{ "--axi-stack-gap": "6px" } as React.CSSProperties}>
              {account.machines.map((m, i) => (
                <div className="axi-row" key={`${m.name}-${i}`}>
                  <span>{m.name}</span>
                  {m.thisMachine && (
                    <span className="axi-chip axi-chip--meta">This machine</span>
                  )}
                </div>
              ))}
            </div>
          )}
          {/* A lapsed sign-in looks exactly like your machines not being
              yours, so it gets a notice rather than a dim line. */}
          {account.expired && !code && (
            <div className="axi-notice axi-notice--danger">
              <span className="axi-notice__icon" aria-hidden="true">
                !
              </span>
              <div className="axi-stack">
                <p>{account.expired}</p>
                {/* The notice says "sign in again", so the button is here.
                    Signing in as yourself keeps this machine's place on the
                    account; nobody should have to sign out first. */}
                <div className="axi-row">
                  <button
                    type="button"
                    className="axi-btn axi-btn--primary axi-btn--sm"
                    disabled={busy}
                    onClick={start}
                  >
                    {busy ? "Starting" : "Sign in again"}
                  </button>
                </div>
              </div>
            </div>
          )}
          {!account.persistent && (
            <p className="axi-ink-dim">
              No system keystore was available, so you will have to sign in
              again the next time Dusk starts.
            </p>
          )}
        </>
      )}

      {account && !user && !code && account.unreadable && (
        <div className="axi-notice axi-notice--danger">
          <span className="axi-notice__icon" aria-hidden="true">
            !
          </span>
          <div className="axi-stack">
            <p>{account.unreadable}</p>
            <div className="axi-row">
              <button
                type="button"
                className="axi-btn axi-btn--sm"
                disabled={busy}
                onClick={() => refresh().catch((e) => setError(describe(e)))}
              >
                Try again
              </button>
            </div>
          </div>
        </div>
      )}

      {account && !user && !code && !account.unreadable && (
        <>
          <p>
            Sign in on each of your machines and they will pair with each
            other without a PIN or an Allow press.
          </p>
          {account.available ? (
            <div className="axi-row">
              <button
                type="button"
                className="axi-btn axi-btn--primary"
                disabled={busy}
                onClick={start}
              >
                {busy ? "Starting" : "Sign in with GitHub"}
              </button>
            </div>
          ) : (
            <p className="axi-ink-dim">
              This build of Dusk was made without GitHub sign-in.
            </p>
          )}
          <p className="axi-ink-faint">
            Dusk asks GitHub only for permission to keep a private list of
            your machines, which holds each one&rsquo;s name and public key.
          </p>
        </>
      )}

      {code && (
        <div className="axi-well axi-stack">
          <p>Enter this code on GitHub to finish signing in.</p>
          <p className="dusk-pin">{code.userCode}</p>
          <div className="axi-row">
            <button
              type="button"
              className="axi-btn"
              onClick={() => openSignInPage().catch((e) => setError(describe(e)))}
            >
              Open GitHub
            </button>
            <button type="button" className="axi-btn axi-btn--ghost" onClick={stop}>
              Cancel
            </button>
            <span className="axi-ink-faint dusk-row__end">
              {code.verificationUrl.replace("https://", "")}
            </span>
          </div>
        </div>
      )}

      {error && (
        <div className="axi-notice axi-notice--danger">
          <span className="axi-notice__icon" aria-hidden="true">
            !
          </span>
          <p>{error}</p>
        </div>
      )}
    </div>
  );
}
