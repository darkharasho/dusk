import { useEffect, useRef, useState } from "react";
import {
  answerPairRequest,
  onPairRequest,
  onPairRequestEnded,
  type PairRequest,
} from "../api";
import { describe } from "../errors";

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
  const [answering, setAnswering] = useState(false);
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
    // A second press while the first is on its way would be answered with
    // "that request has already gone away", in a dialog that is now empty.
    if (!request || answering) return;
    setAnswering(true);
    try {
      await answerPairRequest(request.id, allow);
      setRequest(null);
    } catch (err) {
      setError(describe(err));
    } finally {
      setAnswering(false);
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
            <p className="axi-ink-faint">
              Asking from {request.address}. Dusk declines on its own if nobody
              answers within a minute and a half.
            </p>
            {error && <p className="axi-ink-danger">{error}</p>}
          </div>
          <div className="axi-modal__foot">
            <button
              type="button"
              className="axi-btn axi-btn--ghost"
              disabled={answering}
              onClick={() => answer(false)}
            >
              Don&rsquo;t allow
            </button>
            <button
              type="button"
              className="axi-btn axi-btn--primary"
              disabled={answering}
              onClick={() => answer(true)}
            >
              {answering ? "Allowing" : "Allow"}
            </button>
          </div>
        </>
      )}
    </dialog>
  );
}
