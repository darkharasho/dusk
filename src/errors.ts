/**
 * Text for whatever a failed call threw.
 *
 * Dusk's own commands reject with a sentence meant for the screen, and that
 * is shown as it is. Anything else that can land in a catch — an `Error`
 * from a plugin, an object a plugin rejected with — is reduced to its
 * message rather than to "[object Object]", which is what `String(err)` made
 * of it.
 */
export function describe(err: unknown): string {
  if (typeof err === "string") return err;
  if (err instanceof Error) return err.message;
  if (err && typeof err === "object") {
    const message = (err as { message?: unknown }).message;
    if (typeof message === "string" && message) return message;
    try {
      return JSON.stringify(err);
    } catch {
      return String(err);
    }
  }
  return String(err);
}
