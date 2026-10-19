import { useEffect, useRef, useState } from "react";
import {
  answerPairRequest,
  onPairRequest,
  onPairRequestEnded,
  type PairRequest,
} from "../api";

/**
 * Another machine's Dusk asking to pair with this one.
 *
 * This press replaces typing a PIN. It is the one thing that stops anything
 * on the network pairing itself to this machine unattended, so it names the
 * machine asking and defaults to nothing: closing the prompt declines.
 */
export function PairRequestPrompt() {
  const [request, setRequest] = useState<PairRequest | null>(null);
  const [error, setError] = useState<string | null>(null);
  const dialogRef = useRef<HTMLDialogElement>(null);

  useEffect(() => {
    let stops: Array<() => void> = [];
    let cancelled = false;
    Promise.all([
      onPairRequest((r) => {
        setError(null);
        setRequest(r);
      }),
      onPairRequestEnded((id) =>
        setRequest((current) => (current?.id === id ? null : current)),
      ),
    ]).then((fns) => {
      if (cancelled) fns.forEach((fn) => fn());
      else stops = fns;
    });
    return () => {
      cancelled = true;
      stops.forEach((fn) => fn());
    };
  }, []);

  useEffect(() => {
    const dialog = dialogRef.current;
    if (request && dialog && !dialog.open) dialog.showModal();
    if (!request && dialog?.open) dialog.close();
  }, [request]);

  async function answer(allow: boolean) {
    if (!request) return;
    try {
      await answerPairRequest(request.id, allow);
      setRequest(null);
    } catch (err) {
      setError(String(err));
    }
  }

  return (
    <dialog
      className="axi-modal"
      ref={dialogRef}
      onCancel={(e) => {
        e.preventDefault();
        void answer(false);
      }}
    >
      {request && (
        <>
          <div className="axi-modal__head">
            <h2>{request.name} wants to connect</h2>
          </div>
          <div className="axi-modal__body axi-stack">
            <p>
              Allow it to stream from this machine? It will be able to see this
              screen and control it whenever it connects, until you remove it.
            </p>
            <p className="axi-ink-faint">Asking from {request.address}</p>
            {error && <p className="axi-ink-danger">{error}</p>}
          </div>
          <div className="axi-modal__foot">
            <button
              type="button"
              className="axi-btn axi-btn--ghost"
              onClick={() => answer(false)}
            >
              Don&rsquo;t allow
            </button>
            <button
              type="button"
              className="axi-btn axi-btn--primary"
              onClick={() => answer(true)}
            >
              Allow
            </button>
          </div>
        </>
      )}
    </dialog>
  );
}
