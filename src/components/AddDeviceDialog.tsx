import { useEffect, useRef, useState } from "react";

interface Props {
  onAdd(address: string, name?: string): Promise<void>;
  onClose(): void;
}

/**
 * The manual address book. mDNS does not cross most VPN links, so a machine
 * reached over Tailscale or WireGuard never broadcasts — typing its address
 * is the normal path, not the fallback.
 *
 * A real <dialog> opened with showModal(), which brings the top layer, the
 * focus trap and Escape without any of it being written here.
 */
export function AddDeviceDialog({ onAdd, onClose }: Props) {
  const [address, setAddress] = useState("");
  const [name, setName] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const dialogRef = useRef<HTMLDialogElement>(null);

  useEffect(() => {
    dialogRef.current?.showModal();
  }, []);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (!address.trim() || busy) return;
    setBusy(true);
    setError(null);
    try {
      await onAdd(address.trim(), name.trim() || undefined);
      onClose();
    } catch (err) {
      setError(String(err));
      setBusy(false);
    }
  }

  return (
    <dialog className="axi-modal" ref={dialogRef} onCancel={onClose} onClose={onClose}>
      <form onSubmit={submit}>
        <div className="axi-modal__head">
          <h2>Add a machine</h2>
        </div>

        <div className="axi-modal__body axi-stack">
          <p>
            For machines Dusk cannot see on its own, like one reached over a
            VPN. Use a hostname or IP; the port is optional.
          </p>

          <div className="dusk-field">
            <label htmlFor="address">Address</label>
            <input
              id="address"
              className="axi-input"
              value={address}
              onChange={(e) => setAddress(e.target.value)}
              placeholder="192.168.1.40"
              spellCheck={false}
              autoComplete="off"
              autoFocus
            />
          </div>

          <div className="dusk-field">
            <label htmlFor="name">Name (optional)</label>
            <input
              id="name"
              className="axi-input"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="Workshop PC"
              autoComplete="off"
            />
          </div>

          {error && <p className="axi-ink-danger">{error}</p>}
        </div>

        <div className="axi-modal__foot">
          <button type="button" className="axi-btn axi-btn--ghost" onClick={onClose}>
            Cancel
          </button>
          <button
            type="submit"
            className="axi-btn axi-btn--primary"
            disabled={busy || !address.trim()}
          >
            {busy ? "Adding" : "Add machine"}
          </button>
        </div>
      </form>
    </dialog>
  );
}
