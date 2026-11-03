/**
 * The key combo that opens the in-stream overlay, in the words of the
 * machine the keys are pressed on.
 *
 * The combo lives in the streaming client, beside its other Ctrl+Alt+Shift
 * combos, and Dusk only names it. Named in one place because two screens
 * quote it: Streaming settings, where holding the pointer inside a stream is
 * what makes the floating handle unclickable, and the drawer while a stream
 * is up. A combo that drifts from the client is worse than no combo at all,
 * so there is exactly one string to change if the fork ever moves it.
 */
export const OVERLAY_HOTKEY = /Mac/.test(navigator.userAgent)
  ? "Ctrl + Option + Shift + O"
  : "Ctrl + Alt + Shift + O";
